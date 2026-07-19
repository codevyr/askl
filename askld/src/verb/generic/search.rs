//! Full-text content search verb.
//!
//! `search("query"[, case=..., whole_word=..., limit=N])` materialises an
//! ephemeral layer whose instances point at every byte-range occurrence of
//! `query` inside indexed source content, up to `limit` matches.
//!
//! **NO REGEX.** The query is matched as a literal string by all four
//! variants (substring / whole-word × case-sensitive / -insensitive).
//! Patterns that look regex-ish (e.g. `foo.*bar`, `[a-z]+`) are searched
//! verbatim.  Document prominently in user-facing docs.
//!
//! Step 6 (this file) wires up the verb with hard-coded defaults:
//!   * `case="insensitive"`
//!   * `whole_word="false"` (substring)
//!   * `limit=500`
//!
//! Subsequent steps add full argument parsing (smart-case, `case=`,
//! `whole_word=`, `limit=`) and the truncation warning.

use crate::cfg::ControlFlowGraph;
use crate::span::Span;
use crate::verb::LayerSpec;
use anyhow::{bail, Result};
use async_trait::async_trait;
use index::db_diesel::{
    CompositeFilter, EphContext, EphInstanceRow, EphLayerKind, EphSymbolRow, LayerBatch,
    INSTANCE_TYPE_DEFINITION, SYMBOL_TYPE_CONTENT,
};
use index::symbols::symbol_path_and_leaf;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use super::super::{DeriveMethod, Selector, Verb};

/// Default cap when the caller omits `limit=`.  Predictable cost; explicit
/// `limit=N` overrides it.  Aligns with how interactive code-search tools
/// (GitHub, Sourcegraph) pace result sets.
const DEFAULT_LIMIT: usize = 500;

/// `search(query, ...)` selector — produces one ephemeral content-anchored
/// symbol per matching project, with N instances per symbol where each
/// instance is one byte-range match.  Implemented entirely in SQL via
/// [`Index::search_content_matches`].
#[derive(Debug)]
pub(in crate::verb) struct SearchSelector {
    span: Span,
    query: String,
    case_sensitive: bool,
    whole_word: bool,
    limit: usize,
}

impl SearchSelector {
    pub(in crate::verb) const NAME: &'static str = "search";

    pub fn new(
        span: Span,
        positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        if positional.len() != 1 {
            bail!("search requires exactly one positional argument: query");
        }
        let query = positional[0].clone();
        if query.trim().is_empty() {
            bail!("search: query must be non-empty");
        }
        if query.chars().count() < 3 {
            bail!("search: query must be at least 3 characters (pg_trgm needs one full trigram for the GIN index)");
        }

        // Smart-case is resolved at parse time so the hash and the SQL
        // variant choice see a concrete bool — different `case=` values
        // that resolve to the same bool share the cache.
        let case_sensitive = match named.get("case").map(String::as_str) {
            None | Some("smart") => query.chars().any(|c| c.is_uppercase()),
            Some("sensitive")   => true,
            Some("insensitive") => false,
            Some(other) => bail!(
                "search: case must be \"smart\", \"sensitive\", or \"insensitive\", got: {:?}",
                other,
            ),
        };

        let whole_word = match named.get("whole_word").map(String::as_str) {
            None | Some("false") => false,
            Some("true")         => true,
            Some(other) => bail!(
                "search: whole_word must be \"true\" or \"false\", got: {:?}",
                other,
            ),
        };

        let limit = match named.get("limit") {
            None => DEFAULT_LIMIT,
            Some(s) => {
                let n: usize = s.parse().map_err(|_| anyhow::anyhow!(
                    "search: limit must be a positive integer, got: {:?}", s,
                ))?;
                if n == 0 {
                    bail!("search: limit must be >= 1");
                }
                n
            }
        };

        // Reject unknown named args so typos surface at parse time rather
        // than silently being ignored.
        const ALLOWED: &[&str] = &["case", "whole_word", "limit"];
        for key in named.keys() {
            if !ALLOWED.contains(&key.as_str()) {
                bail!(
                    "search: unknown argument {:?}; allowed: {:?}",
                    key, ALLOWED,
                );
            }
        }

        Ok(Arc::new(Self {
            span,
            query,
            case_sensitive,
            whole_word,
            limit,
        }))
    }

    /// Sanitise a query for use inside the symbol name.  Replace `:` and
    /// any non-printable / control characters with `?` so the resulting
    /// `search:<query>` name renders cleanly in the UI.  No DB impact —
    /// symbol names are plain text — purely cosmetic.
    fn sanitise_for_symbol_name(q: &str) -> String {
        q.chars()
            .map(|c| if c == ':' || c.is_control() { '?' } else { c })
            .collect()
    }
}

impl Verb for SearchSelector {
    fn name(&self) -> &str {
        SearchSelector::NAME
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
impl Selector for SearchSelector {
    fn has_layer_spec(&self) -> bool { true }

    /// The layer hash mixes the user-visible inputs (query, case,
    /// whole_word, limit) with the canonical hash of the surrounding
    /// command's filters via [`CompositeFilter::hash_into`].  Different
    /// filter compositions therefore produce different cache entries; the
    /// same query under the same filter set hits the cache.  Stale entries
    /// are wiped by `purge_eph_cache` on each `finalize_project`.
    async fn layer_spec(
        &self,
        cfg: &ControlFlowGraph,
        eph: &EphContext,
        composite_filter: &CompositeFilter,
        _resolved: &crate::verb::LabelResolutions,
    ) -> Result<Option<LayerSpec>> {
        // 1. Cache key over inputs + filter set.
        let mut hasher = Sha256::new();
        hasher.update(eph.last().unwrap_or(0i64).to_le_bytes());
        hasher.update(EphLayerKind::Search.as_str().as_bytes());
        hasher.update((self.query.len() as u64).to_le_bytes());
        hasher.update(self.query.as_bytes());
        hasher.update([self.case_sensitive as u8]);
        hasher.update([self.whole_word as u8]);
        hasher.update((self.limit as u64).to_le_bytes());
        composite_filter.hash_into(&mut hasher);
        let hash: [u8; 32] = hasher.finalize().into();

        // 2. Run the SQL.  All filtering, matching, and byte-range
        //    extraction happens inside one of four straight-line SQL
        //    variants picked from (whole_word, case_sensitive).
        let (matches, truncated) = cfg.index
            .search_content_matches(
                &self.query,
                self.case_sensitive,
                self.whole_word,
                composite_filter,
                self.limit,
            )
            .await?;

        // 3. Group matches by project_id so we can emit one ephemeral
        //    symbol per project (`symbols.project_id` is NOT NULL).
        //    Materialise the populate inputs synchronously; the closure
        //    only sees `Send` data so it stays cheap to .await past.
        struct GroupedMatches {
            project_id: i32,
            ranges: Vec<(i32 /* object_id */, i32 /* start_byte */, i32 /* end_byte */)>,
        }
        let mut by_project: HashMap<i32, Vec<(i32, i32, i32)>> = HashMap::new();
        for m in matches {
            by_project.entry(m.project_id).or_default().push((m.object_id, m.start_byte, m.end_byte));
        }
        let mut groups: Vec<GroupedMatches> = by_project
            .into_iter()
            .map(|(project_id, ranges)| GroupedMatches { project_id, ranges })
            .collect();
        // Determinism for the populate batch — same input → same insert
        // order → same symbol/instance ids.
        groups.sort_by_key(|g| g.project_id);

        let sym_name = format!("search:{}", Self::sanitise_for_symbol_name(&self.query));
        let (sym_path, sym_leaf) = symbol_path_and_leaf(&sym_name, SYMBOL_TYPE_CONTENT);

        let populate: crate::verb::LayerPopulate = Box::new(move |txn| Box::pin(async move {
            // 3a. One ephemeral symbol per project_id.
            let mut sym_batch = LayerBatch::new();
            for g in &groups {
                sym_batch.symbols.push(EphSymbolRow {
                    name: sym_name.clone(),
                    path: sym_path.clone(),
                    project_id: g.project_id,
                    symbol_type: SYMBOL_TYPE_CONTENT,
                    scope: None,
                    leaf_name: sym_leaf.clone(),
                });
            }
            let symbol_ids = txn.insert_batch(&sym_batch).await?;

            // 3b. One ephemeral instance per byte-range match.
            let mut inst_batch = LayerBatch::new();
            for (g, symbol_id) in groups.iter().zip(symbol_ids.iter()) {
                for (object_id, start, end) in &g.ranges {
                    inst_batch.instances.push(EphInstanceRow {
                        symbol_id: *symbol_id,
                        object_id: *object_id,
                        start: *start as i64,
                        end: *end as i64,
                        instance_type: INSTANCE_TYPE_DEFINITION,
                    });
                }
            }
            txn.insert_batch(&inst_batch).await?;

            Ok(truncated)
        }));

        Ok(Some(LayerSpec {
            hash,
            kind: EphLayerKind::Search,
            parent_id: eph.last(),
            populate,
        }))
    }

    /// Reconstruct the truncation warning every time the layer reports
    /// truncated=true.  Cache hits and misses both reach here because
    /// `eph_layers.truncated` is read on both paths.  The verb owns the
    /// wording and uses its own span, so the warning UX is identical
    /// across calls.
    fn make_truncation_warning(&self) -> Option<pest::error::Error<crate::parser::Rule>> {
        Some(pest::error::Error::new_from_span(
            pest::error::ErrorVariant::CustomError {
                message: format!(
                    "search({:?}): result truncated at {} matches; narrow the query \
                     (more specific text, project(\"name\"), whole_word=\"true\")",
                    self.query, self.limit,
                ),
            },
            self.span.as_pest_span(),
        ))
    }
}

impl Display for SearchSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SearchSelector(query={:?}, case_sensitive={}, whole_word={}, limit={})",
            self.query, self.case_sensitive, self.whole_word, self.limit,
        )
    }
}
