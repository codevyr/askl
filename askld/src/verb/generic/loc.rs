use crate::cfg::ControlFlowGraph;
use crate::line_index::{line_to_offset, next_line_offset};
use crate::parser::Value;
use crate::span::Span;
use crate::verb::LayerSpec;
use anyhow::{bail, Result};
use async_trait::async_trait;
use index::db_diesel::{
    EphContext, EphInstanceRow, EphSymbolRow, Index, LayerBatch, INSTANCE_TYPE_DEFINITION,
    SYMBOL_TYPE_CONTENT,
};
use index::symbols::symbol_path_and_leaf;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::sync::{Arc, LazyLock, Mutex};

/// File ids for which we have already emitted the CRLF warning in this
/// process.  Without this, the same Windows-origin file logged a warn
/// on every `loc(...)` call, which drowned operator logs.
static CRLF_WARNED: LazyLock<Mutex<HashSet<i32>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

use super::super::{AnchorKind, DeriveMethod, Selector, Verb};

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
        positional: &Vec<Value>,
        named: &HashMap<String, Value>,
    ) -> Result<Arc<dyn Verb>> {
        if positional.len() < 2 {
            bail!("loc requires two positional arguments: file path and line number");
        }
        let file_path = positional[0].as_plain()?.to_string();
        let line: usize = positional[1]
            .as_plain()?
            .parse()
            .map_err(|_| anyhow::anyhow!("loc line number must be an integer"))?;
        if line == 0 {
            bail!("loc line number must be >= 1");
        }
        let project = named
            .get("project")
            .map(|v| v.as_plain().map(str::to_string))
            .transpose()?;

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

    /// Loc rows are always content-typed; a scope's inherited default type
    /// filter must not be injected into this command (see SearchSelector).
    fn suppresses_default_type_filter(&self) -> bool {
        true
    }

    fn anchor_kind(&self) -> Option<AnchorKind> {
        Some(AnchorKind::Location)
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
    fn has_layer_spec(&self) -> bool {
        true
    }

    /// The input hash is over the request *inputs only* (file_path,
    /// line, project).  It deliberately does **not** depend on the
    /// matched-file set returned by `find_objects_by_path`, so the cache
    /// stays meaningful across repeated calls with the same source text —
    /// and it does not fold the eph chain, so the root shard survives any
    /// upstream ephemeral change (chain dependence lives in the selection
    /// shard the executor creates alongside it).
    ///
    /// Consequence: if the underlying index changes (a new project is
    /// pushed, a file is renamed), a previously-cached layer becomes
    /// stale relative to the new index. Cache freshness is the
    /// responsibility of `IndexStore::finalize_project` and
    /// `delete_project`, which purge the eph cache inside their commit
    /// transactions (with `purge_eph_cache` blocking on in-flight layer
    /// transactions). Anywhere else that mutates the persistent index must
    /// also invalidate, or `loc(...)` calls will return stale rows.
    /// The file-existence / line-range validation below runs only on a
    /// cache MISS by design: with correct invalidation, a hit implies the
    /// layer postdates the last index mutation, so re-validating on hits
    /// would be redundant work.
    async fn layer_spec(
        &self,
        _cfg: &ControlFlowGraph,
        eph: &EphContext,
        _composite_filter: &index::db_diesel::CompositeFilter,
        _resolved: &crate::verb::LabelResolutions,
        // Deliberately unfused: loc is single-file by construction (its path
        // argument already scopes the scan), so container scope-fusion buys
        // nothing — the composition machinery constrains the result instead.
        _parent_scope: &index::db_diesel::ScopeContext,
    ) -> Result<Option<LayerSpec>> {
        // 1. Input hash from inputs only — never the eph chain.
        let mut hasher = Sha256::new();
        // Explicit per-verb domain tag (byte-identical to the former
        // `EphLayerKind::Loc.as_str()`) so loc's hashes stay disjoint even
        // though the layer kind is now the coarse `Ephemeral`.
        hasher.update(b"loc");
        hasher.update((self.file_path.len() as u64).to_le_bytes());
        hasher.update(self.file_path.as_bytes());
        hasher.update((self.line as u64).to_le_bytes());
        match &self.project {
            Some(p) => {
                hasher.update([1u8]);
                hasher.update(p.as_bytes());
            }
            None => {
                hasher.update([0u8]);
            }
        }
        let hash: [u8; 32] = hasher.finalize().into();

        // 2. Everything else is deferred into the populates so it runs only
        //    on a cache miss.  The queries read `objects`/`object_contents`
        //    on the layer transaction's own connection.  The user-facing
        //    bail!s on "no file" / "line out of range" move with them: the
        //    transaction rolls back, the error surfaces identically, and
        //    failed runs never commit a layer (errors stay uncached, as
        //    before).
        //
        //    loc is layer-SHARDED (like search), but its resolution is
        //    GLOBAL across roots on each shard's layer set — NOT per-root
        //    `[root.id]` — so the path/line bails stay functions of global
        //    data and behave uniformly across roots: either every root's
        //    root-shard populate bails (file matches nowhere on the root
        //    layers)
        //    or none does.  Only the batch built from the resolved matches
        //    is scoped to the root's project.
        //
        //    Root shard = GLOBAL resolution over `eph.root_ids()` (persistent
        //    branch, `eph_branch=false`): every persistent object lives on a
        //    root layer and loc had no layer filter before, so the same
        //    objects resolve → the same "no file" bail and the same
        //    instances.
        //    Eph shard = GLOBAL resolution over `eph.visible_ids()`
        //    with the eph-disjointness guard (`objects.layer < 0`,
        //    `eph_branch=true`): there are no ephemeral objects today, so it
        //    resolves empty — and an empty eph resolution is NORMAL, so it
        //    must NOT bail (only the root shard bails "no file"; see
        //    `resolve`).
        struct FileMatch {
            file_id: i32,
            project_id: i32,
            line_start: i64,
            line_end: i64,
        }
        struct LocInputs {
            file_path: String,
            line: usize,
            project: Option<String>,
            sym_name: String,
            sym_path: String,
            sym_leaf: String,
            /// Root-layer id set — the persistent root shard, captured at
            /// `layer_spec` time.  Layer shards bind their own singleton
            /// `[L_j]`, so no chain-wide eph set is needed here.
            root_visible: Vec<i64>,
            /// GLOBAL path/line resolution for the root shard, shared across
            /// its per-root populates: the first root to run resolves (path
            /// query + content reads + line offsets) and memoises; siblings
            /// await the cell instead of re-reading every matching file once
            /// per root.  The "no file" bail lives inside the initialiser, so
            /// error semantics stay uniform across roots, and cache hits skip
            /// resolution entirely.  Layer shards resolve their own `[L_j]`
            /// fresh (distinct sets ⇒ no shared memo).
            root_resolved: tokio::sync::OnceCell<Vec<FileMatch>>,
        }

        /// GLOBAL path/line resolution for one shard.  Identical body to the
        /// previous single-cell resolution, with ONE difference: BOTH bails —
        /// "no file" and "line out of range" — fire only on the root shard
        /// (`!eph_branch`).  On an eph shard an empty resolution is normal: a
        /// layer may simply not carry this path, or may carry a shorter copy
        /// where the line is out of range — in either case the layer just
        /// contributes nothing, so `resolve` returns `Ok(vec![])` rather than
        /// failing the whole statement.  On the root shard both remain hard
        /// user errors against the file the user actually named.
        async fn resolve(
            txn: &mut index::db_diesel::EphTransaction<'_>,
            inputs: &LocInputs,
            visible_layers: &[i64],
            eph_branch: bool,
        ) -> Result<Vec<FileMatch>> {
            let file_path = &inputs.file_path;
            let line = inputs.line;
            let matches = Index::find_objects_by_path_on(
                txn.connection(),
                file_path,
                inputs.project.as_deref(),
                visible_layers,
                eph_branch,
            )
            .await?;

            if matches.is_empty() {
                // Only the root shard bails "no file": an empty eph
                // resolution is normal (there are no ephemeral objects
                // today), so an eph shard returns empty instead.
                if !eph_branch {
                    bail!("loc: no file matching '{}' found in index", file_path);
                }
                return Ok(Vec::new());
            }

            let mut file_matches = Vec::new();
            for (file_id, project_id) in &matches {
                let content = Index::get_file_contents_on(txn.connection(), *file_id).await?;
                let content_bytes = content.as_bytes();

                // CRLF detection: `line_to_offset` recognises only `\n`, so
                // on CRLF files the resolved offset includes the preceding
                // `\r` in the line above.  Emit a one-shot warning per
                // affected file so operators can spot the discrepancy; offset
                // semantics stay LF-based.
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

                let line_start = match line_to_offset(content_bytes, line) {
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
                // Only the root shard treats an out-of-range line as a user
                // error; on an eph shard the layer may hold a shorter copy of
                // the file, which just means this layer contributes no match.
                if !eph_branch {
                    bail!(
                        "loc: line {} out of range for all files matching '{}'",
                        line,
                        file_path
                    );
                }
                return Ok(Vec::new());
            }
            Ok(file_matches)
        }

        /// Build the per-root symbol + instance batches from a shard's
        /// GLOBAL resolution.  Shared by both shards: identical instance
        /// construction, scoped to this root's project.
        async fn build_instances(
            txn: &mut index::db_diesel::EphTransaction<'_>,
            inputs: &LocInputs,
            file_matches: &[FileMatch],
            root: &index::db_diesel::RootLayer,
        ) -> Result<()> {
            // This root's share of the resolved matches.  Constructed (not
            // post-filtered) per root so the returned symbol ids stay 1:1
            // with the instance rows built from them; the scoped insert's SQL
            // filter is then a consistency no-op.  An empty share is the
            // uniform empty share for this root on this shard.
            let root_matches: Vec<&FileMatch> = file_matches
                .iter()
                .filter(|fm| fm.project_id == root.project_id)
                .collect();

            // Symbol IDs are only known after insertion, so insert symbols
            // first, then build the instance batch from the returned IDs.
            let mut sym_batch = LayerBatch::new();
            for fm in &root_matches {
                sym_batch.symbols.push(EphSymbolRow {
                    name: inputs.sym_name.clone(),
                    path: inputs.sym_path.clone(),
                    project_id: fm.project_id,
                    symbol_type: SYMBOL_TYPE_CONTENT,
                    scope: None,
                    leaf_name: inputs.sym_leaf.clone(),
                });
            }
            let symbol_ids = txn.insert_batch(&sym_batch, root.project_id).await?;

            let mut inst_batch = LayerBatch::new();
            for (fm, symbol_id) in root_matches.iter().zip(symbol_ids.iter()) {
                inst_batch.instances.push(EphInstanceRow {
                    symbol_id: *symbol_id,
                    object_id: fm.file_id,
                    start: fm.line_start,
                    end: fm.line_end,
                    instance_type: INSTANCE_TYPE_DEFINITION,
                });
            }
            txn.insert_batch(&inst_batch, root.project_id).await?;
            Ok(())
        }

        let sym_name = format!("loc:{}:{}", self.file_path, self.line);
        let (sym_path, sym_leaf) = symbol_path_and_leaf(&sym_name, SYMBOL_TYPE_CONTENT);
        let inputs = std::sync::Arc::new(LocInputs {
            file_path: self.file_path.clone(),
            line: self.line,
            project: self.project.clone(),
            sym_name,
            sym_path,
            sym_leaf,
            root_visible: eph.root_ids(),
            root_resolved: tokio::sync::OnceCell::new(),
        });

        let root_shard_inputs = std::sync::Arc::clone(&inputs);
        let root_shard_populate: crate::verb::RootShardPopulate = Box::new(move |txn, root| {
            let inputs = std::sync::Arc::clone(&root_shard_inputs);
            Box::pin(async move {
                let file_matches: &Vec<FileMatch> = inputs
                    .root_resolved
                    .get_or_try_init(|| resolve(txn, &inputs, &inputs.root_visible, false))
                    .await?;
                build_instances(txn, &inputs, file_matches, root).await?;
                // loc never truncates; truncated = false.
                Ok(false)
            })
        });

        // The selection shard is a NO-OP: the layer shards below carry every
        // eph content layer's matches, so a scanning selection shard would
        // double-count.  It survives empty as the chain-continuation marker.
        let selection_shard_populate: crate::verb::SelectionShardPopulate =
            Box::new(|_txn, _root, _root_shard_ref| Box::pin(async { Ok(false) }));

        let layer_shard_inputs = std::sync::Arc::clone(&inputs);
        let layer_shard_populate: crate::verb::LayerShardPopulate =
            Box::new(move |txn, root, layer_id, _root_shard_ref| {
                let inputs = std::sync::Arc::clone(&layer_shard_inputs);
                Box::pin(async move {
                    // N-way: each layer shard resolves ONE eph content layer.
                    // No OnceCell — layer shards are distinct sets, so resolve
                    // fresh per `L_j` (cheap; empty until eph content exists).
                    let file_matches = resolve(txn, &inputs, &[layer_id], true).await?;
                    build_instances(txn, &inputs, &file_matches, root).await?;
                    // loc never truncates; truncated = false.
                    Ok(false)
                })
            });

        // loc is layer-sharded: the root shard scans root layers (persistent)
        // and each layer shard scans ONE eph content layer
        // (`objects.layer < 0`); the selection shard is an empty chain marker.
        // With no ephemeral objects today the eph shards resolve empty, so this
        // is behaviourally identical to the old persistent-only spec — but the
        // shape is uniform with search and future ephemeral objects flow
        // through automatically.  `selection_extra` stays empty: the delta is
        // fully determined by (parent chain, root shard).
        Ok(Some(LayerSpec {
            input_hash: hash,
            root_shard_populate,
            selection_shard_populate,
            layer_shard_populate: Some(layer_shard_populate),
            selection_extra: Vec::new(),
        }))
    }
}

impl Display for LocSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "LocSelector(file={}, line={})",
            self.file_path, self.line
        )
    }
}
