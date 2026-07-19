use crate::cfg::ControlFlowGraph;
use crate::span::Span;
use crate::verb::LayerSpec;
use anyhow::{bail, Result};
use async_trait::async_trait;
use index::db_diesel::{
    EphContext, EphInstanceRow, EphLayerKind, EphSymbolRow, LayerBatch,
    INSTANCE_TYPE_DEFINITION, SYMBOL_TYPE_CONTENT,
};
use index::symbols::symbol_path_and_leaf;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::sync::{Arc, LazyLock, Mutex};

/// File ids for which we have already emitted the CRLF warning in this
/// process.  Without this, the same Windows-origin file logged a warn
/// on every `loc(...)` call, which drowned operator logs.
static CRLF_WARNED: LazyLock<Mutex<HashSet<i32>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

use super::super::{DeriveMethod, Selector, Verb};

/// LocSelector - creates an ephemeral symbol at a specific file location.
///
/// Usage: loc("path/to/file.c", "42")
///   positional[0] = file path (suffix match)
///   positional[1] = line number (1-based)
///   named: project="name" (optional)
///
/// Content-addressed via SHA-256 hash for caching.  The layer is materialized
/// by the statement-execution layer through [`Selector::layer_spec`]; this
/// type holds only immutable input data.
#[derive(Debug)]
pub(in crate::verb) struct LocSelector {
    span: Span,
    file_path: String,
    line: usize,
    project: Option<String>,
}

impl LocSelector {
    pub(in crate::verb) const NAME: &'static str = "loc";

    pub fn new(
        span: Span,
        positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        if positional.len() < 2 {
            bail!("loc requires two positional arguments: file path and line number");
        }
        let file_path = positional[0].clone();
        let line: usize = positional[1].parse()
            .map_err(|_| anyhow::anyhow!("loc line number must be an integer"))?;
        if line == 0 {
            bail!("loc line number must be >= 1");
        }
        let project = named.get("project").cloned();

        Ok(Arc::new(Self {
            span,
            file_path,
            line,
            project,
        }))
    }

}

impl Verb for LocSelector {
    fn name(&self) -> &str {
        LocSelector::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        Ok(self)
    }
}

#[async_trait(?Send)]
impl Selector for LocSelector {
    fn has_layer_spec(&self) -> bool { true }

    /// The layer hash is over the request *inputs only* (file_path, line,
    /// project, parent layer id).  It deliberately does **not** depend on
    /// the matched-file set returned by `find_objects_by_path`, so the
    /// cache stays meaningful across repeated calls with the same source
    /// text.
    ///
    /// Consequence: if the underlying index changes (a new project is
    /// pushed, a file is renamed), a previously-cached layer becomes
    /// stale relative to the new index. Cache freshness is the
    /// responsibility of `IndexStore::finalize_project`, which deletes
    /// `index.eph_layers WHERE kind != 'canary'` inside its commit
    /// transaction. Anywhere else that mutates the persistent index must
    /// also invalidate, or `loc(...)` calls will return stale rows.
    async fn layer_spec(
        &self,
        cfg: &ControlFlowGraph,
        eph: &EphContext,
        _composite_filter: &index::db_diesel::CompositeFilter,
        _resolved: &crate::verb::LabelResolutions,
    ) -> Result<Option<LayerSpec>> {
        // 1. Compute content-addressed hash from inputs only.
        let mut hasher = Sha256::new();
        hasher.update(eph.last().unwrap_or(0i64).to_le_bytes());
        hasher.update(EphLayerKind::Loc.as_str().as_bytes());
        hasher.update((self.file_path.len() as u64).to_le_bytes());
        hasher.update(self.file_path.as_bytes());
        hasher.update((self.line as u64).to_le_bytes());
        match &self.project {
            Some(p) => { hasher.update([1u8]); hasher.update(p.as_bytes()); }
            None    => { hasher.update([0u8]); }
        }
        let hash: [u8; 32] = hasher.finalize().into();

        // 2. Resolve file paths and compute byte offsets.  These read from
        //    `objects`/`object_contents`, which have no `eph_layer` column,
        //    so results are deterministic regardless of in-flight transactions.
        let matches = cfg.index.find_objects_by_path(
            &self.file_path,
            self.project.as_deref(),
        ).await?;

        if matches.is_empty() {
            bail!("loc: no file matching '{}' found in index", self.file_path);
        }

        struct FileMatch {
            file_id: i32,
            project_id: i32,
            line_start: i64,
            line_end: i64,
        }
        let mut file_matches = Vec::new();
        for (file_id, project_id) in &matches {
            let content = cfg.index.get_file_contents(*file_id).await?;
            let content_bytes = content.as_bytes();

            // CRLF detection: `line_to_offset` recognises only `\n`, so on CRLF
            // files the resolved offset includes the preceding `\r` in the line
            // above.  Emit a one-shot warning per affected file so operators
            // can spot the discrepancy; offset semantics stay LF-based.
            if content_bytes.contains(&b'\r') {
                let fid: i32 = (*file_id).into();
                let first_seen = CRLF_WARNED.lock().unwrap().insert(fid);
                if first_seen {
                    tracing::warn!(
                        file_id = fid,
                        "loc: file contains CR bytes; line offsets are LF-based and may be off by one per CRLF"
                    );
                }
            }

            let line_start = match line_to_offset(content_bytes, self.line) {
                Some(offset) => offset,
                None => continue,
            };
            let line_end = next_line_offset(content_bytes, line_start);

            file_matches.push(FileMatch {
                file_id: (*file_id).into(),
                project_id: (*project_id).into(),
                line_start,
                line_end,
            });
        }

        if file_matches.is_empty() {
            bail!("loc: line {} out of range for all files matching '{}'", self.line, self.file_path);
        }

        // 3. Build the populate closure.  Symbol IDs are only known after
        //    insertion, so we insert symbols first, then build the instance
        //    batch from the returned IDs.
        let sym_name = format!("loc:{}:{}", self.file_path, self.line);
        let (sym_path, sym_leaf) = symbol_path_and_leaf(&sym_name, SYMBOL_TYPE_CONTENT);

        let populate: crate::verb::LayerPopulate = Box::new(move |txn| Box::pin(async move {
            let mut sym_batch = LayerBatch::new();
            for fm in &file_matches {
                sym_batch.symbols.push(EphSymbolRow {
                    name: sym_name.clone(),
                    path: sym_path.clone(),
                    project_id: fm.project_id,
                    symbol_type: SYMBOL_TYPE_CONTENT,
                    scope: None,
                    leaf_name: sym_leaf.clone(),
                });
            }
            let symbol_ids = txn.insert_batch(&sym_batch).await?;

            let mut inst_batch = LayerBatch::new();
            for (fm, symbol_id) in file_matches.iter().zip(symbol_ids.iter()) {
                inst_batch.instances.push(EphInstanceRow {
                    symbol_id: *symbol_id,
                    object_id: fm.file_id,
                    start: fm.line_start,
                    end: fm.line_end,
                    instance_type: INSTANCE_TYPE_DEFINITION,
                });
            }
            txn.insert_batch(&inst_batch).await?;
            // loc never truncates; truncated = false.
            Ok(false)
        }));

        Ok(Some(LayerSpec {
            hash,
            kind: EphLayerKind::Loc,
            parent_id: eph.last(),
            populate,
        }))
    }
}

impl Display for LocSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LocSelector(file={}, line={})", self.file_path, self.line)
    }
}

/// Convert a 1-based line number to a byte offset (start of line).
fn line_to_offset(content: &[u8], line: usize) -> Option<i64> {
    if line == 0 {
        return None;
    }
    if line == 1 {
        return Some(0);
    }

    let mut current_line = 1usize;
    for (idx, byte) in content.iter().enumerate() {
        if *byte == b'\n' {
            current_line += 1;
            if current_line == line {
                return Some((idx + 1) as i64);
            }
        }
    }
    None
}

/// Find the byte offset of the next newline after `start`, or end of content.
fn next_line_offset(content: &[u8], start: i64) -> i64 {
    let start = start as usize;
    for idx in start..content.len() {
        if content[idx] == b'\n' {
            return idx as i64;
        }
    }
    content.len() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_to_offset_line_1() {
        assert_eq!(line_to_offset(b"hello\nworld\n", 1), Some(0));
    }

    #[test]
    fn line_to_offset_line_2() {
        assert_eq!(line_to_offset(b"hello\nworld\n", 2), Some(6));
    }

    #[test]
    fn line_to_offset_line_3() {
        assert_eq!(line_to_offset(b"hello\nworld\n", 3), Some(12));
    }

    #[test]
    fn line_to_offset_line_0_returns_none() {
        assert_eq!(line_to_offset(b"hello\n", 0), None);
    }

    #[test]
    fn line_to_offset_past_end_returns_none() {
        assert_eq!(line_to_offset(b"hello\n", 3), None);
    }

    #[test]
    fn line_to_offset_empty_content() {
        assert_eq!(line_to_offset(b"", 1), Some(0));
        assert_eq!(line_to_offset(b"", 2), None);
    }

    #[test]
    fn next_line_offset_finds_newline() {
        assert_eq!(next_line_offset(b"hello\nworld\n", 0), 5);
    }

    #[test]
    fn next_line_offset_at_end_of_content() {
        assert_eq!(next_line_offset(b"hello", 0), 5);
    }

    #[test]
    fn next_line_offset_from_mid_line() {
        assert_eq!(next_line_offset(b"hello\nworld\n", 6), 11);
    }
}
