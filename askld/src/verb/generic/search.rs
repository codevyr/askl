//! Full-text content search verb.
//!
//! `search("query"[, case=..., whole_word=..., limit=N])` materialises one
//! ephemeral layer per visible project, whose instances point at every
//! byte-range occurrence of `query` inside that project's indexed source
//! content, up to `limit` matches PER PROJECT.  The per-project cap is
//! deliberate (not just cosmetic): each project's layer is cached
//! independently of co-visible projects, so its contents cannot depend on
//! how many matches other projects have.  The union across projects is
//! therefore bounded by `limit × visible projects`, not `limit`.
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
use crate::parser::Value;
use crate::span::Span;
use crate::verb::LayerSpec;
use anyhow::{bail, Result};
use async_trait::async_trait;
use index::db_diesel::{
    CompositeFilter, EphContext, EphInstanceRow, EphSymbolRow, Index, LayerBatch,
    INSTANCE_TYPE_DEFINITION, SYMBOL_TYPE_CONTENT,
};
use index::symbols::{smart_case_sensitive, symbol_path_and_leaf};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use super::super::{DeriveMethod, Selector, Verb};

/// Default per-project cap when the caller omits `limit=`.  Predictable
/// cost; explicit `limit=N` overrides it.  Aligns with how interactive
/// code-search tools (GitHub, Sourcegraph) pace result sets.
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
        positional: &Vec<Value>,
        named: &HashMap<String, Value>,
    ) -> Result<Arc<dyn Verb>> {
        if positional.len() != 1 {
            bail!("search requires exactly one positional argument: query");
        }
        let query = positional[0].as_plain()?.to_string();
        if query.trim().is_empty() {
            bail!("search: query must be non-empty");
        }
        if query.chars().count() < 3 {
            bail!("search: query must be at least 3 characters (pg_trgm needs one full trigram for the GIN index)");
        }

        // Smart-case is resolved at parse time so the hash and the SQL
        // variant choice see a concrete bool — different `case=` values
        // that resolve to the same bool share the cache.
        let case_sensitive = match crate::parser::named_plain(named, "case")? {
            None | Some("smart") => smart_case_sensitive(&query),
            Some("sensitive") => true,
            Some("insensitive") => false,
            Some(other) => bail!(
                "search: case must be \"smart\", \"sensitive\", or \"insensitive\", got: {:?}",
                other,
            ),
        };

        let whole_word = match crate::parser::named_plain(named, "whole_word")? {
            None | Some("false") => false,
            Some("true") => true,
            Some(other) => bail!(
                "search: whole_word must be \"true\" or \"false\", got: {:?}",
                other,
            ),
        };

        let limit = match named.get("limit") {
            None => DEFAULT_LIMIT,
            Some(s) => {
                let s = s.as_plain()?;
                let n: usize = s.parse().map_err(|_| {
                    anyhow::anyhow!("search: limit must be a positive integer, got: {:?}", s,)
                })?;
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
                bail!("search: unknown argument {:?}; allowed: {:?}", key, ALLOWED,);
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
    fn has_layer_spec(&self) -> bool {
        true
    }

    /// The base-layer hash mixes the user-visible inputs (query, case,
    /// whole_word, limit) with the canonical hash of the surrounding
    /// command's filters via [`CompositeFilter::hash_into`].  Different
    /// filter compositions therefore produce different cache entries; the
    /// same query under the same filter set hits the cache — under ANY
    /// upstream ephemeral context, since the base hash deliberately does
    /// not fold the eph chain.  Any eph-content dependence lives in the
    /// per-layer atoms the executor creates alongside the base (each keyed on
    /// its own layer, not the chain).  Stale entries are wiped by
    /// `purge_eph_cache` on each `finalize_project`.
    async fn layer_spec(
        &self,
        _cfg: &ControlFlowGraph,
        _eph: &EphContext,
        composite_filter: &CompositeFilter,
        _resolved: &crate::verb::LabelResolutions,
    ) -> Result<Option<LayerSpec>> {
        // 1. Base cache key over inputs + filter set — never the eph chain.
        let mut hasher = Sha256::new();
        // Explicit per-verb domain tag (kept byte-identical to the former
        // `EphLayerKind::Search.as_str()`) so hashes stay disjoint from other
        // verbs even though the layer kind is now the coarse `Ephemeral`.
        hasher.update(b"search");
        hasher.update((self.query.len() as u64).to_le_bytes());
        hasher.update(self.query.as_bytes());
        hasher.update([self.case_sensitive as u8]);
        hasher.update([self.whole_word as u8]);
        hasher.update((self.limit as u64).to_le_bytes());
        composite_filter.hash_into(&mut hasher);
        let hash: [u8; 32] = hasher.finalize().into();

        // 2. Everything expensive is deferred into the populate closure so
        //    it runs only on a cache miss; on a hit the layer's previously
        //    materialised rows are served without touching content_store.
        //    The closure runs once PER ROOT (`Fn`), so it shares its inputs
        //    via `Arc` and clones the handle into each returned future; the
        //    SQL runs on the layer transaction's own connection — no second
        //    pool checkout while the row lock is held.
        struct SearchInputs {
            query: String,
            case_sensitive: bool,
            whole_word: bool,
            limit: usize,
            filter: CompositeFilter,
            sym_name: String,
            sym_path: String,
            sym_leaf: String,
        }
        let sym_name = format!("search:{}", Self::sanitise_for_symbol_name(&self.query));
        let (sym_path, sym_leaf) = symbol_path_and_leaf(&sym_name, SYMBOL_TYPE_CONTENT);
        let inputs = std::sync::Arc::new(SearchInputs {
            query: self.query.clone(),
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            limit: self.limit,
            filter: composite_filter.clone(),
            sym_name,
            sym_path,
            sym_leaf,
        });

        let scan: crate::verb::ShardedScan =
            Box::new(move |txn, root, visible_layers, eph_branch| {
                let inputs = std::sync::Arc::clone(&inputs);
                Box::pin(async move {
                    // 2a. Run the SQL for THIS root's project.  All filtering,
                    //     matching, and byte-range extraction happens inside one
                    //     of four straight-line SQL variants picked from
                    //     (whole_word, case_sensitive); the project scope is an
                    //     always-present bind, and the limit caps matches PER
                    //     PROJECT — required for cache correctness (this root's
                    //     base content must not depend on co-visible projects).
                    //     `visible_layers`/`eph_branch` select the shard: the base
                    //     shard scans persistent content (`[root.id]`, false), the
                    //     supplement shard scans eph-layer content (the upstream
                    //     chain, true) — the executor picks which via
                    //     `LayerSpec::sharded_scan`.
                    let (matches, truncated) = Index::search_content_matches_on(
                        txn.connection(),
                        &inputs.query,
                        inputs.case_sensitive,
                        inputs.whole_word,
                        &inputs.filter,
                        inputs.limit,
                        root.project_id,
                        &visible_layers,
                        eph_branch,
                    )
                    .await?;

                    if matches.is_empty() {
                        // Uniform empty base: no matches in this project (or the
                        // filter excludes it) — the layer row itself is still
                        // materialised by the executor.
                        return Ok(truncated);
                    }

                    // 2b. One ephemeral symbol for this root's project, then one
                    //     ephemeral instance per byte-range match.  SQL result
                    //     order (object_id, start) keeps the batch deterministic.
                    let mut sym_batch = LayerBatch::new();
                    sym_batch.symbols.push(EphSymbolRow {
                        name: inputs.sym_name.clone(),
                        path: inputs.sym_path.clone(),
                        project_id: root.project_id,
                        symbol_type: SYMBOL_TYPE_CONTENT,
                        scope: None,
                        leaf_name: inputs.sym_leaf.clone(),
                    });
                    let symbol_ids = txn.insert_batch(&sym_batch, root.project_id).await?;
                    let symbol_id = symbol_ids[0];

                    let mut inst_batch = LayerBatch::new();
                    for m in &matches {
                        inst_batch.instances.push(EphInstanceRow {
                            symbol_id,
                            object_id: m.object_id,
                            start: m.start_byte as i64,
                            end: m.end_byte as i64,
                            instance_type: INSTANCE_TYPE_DEFINITION,
                        });
                    }
                    txn.insert_batch(&inst_batch, root.project_id).await?;

                    Ok(truncated)
                })
            });

        // The executor SHARDS this scan by layer: the base shard
        // (`vec![root.id]`, `eph_branch=false`) over persistent content, and
        // one per-layer atom (`vec![L_j]`, `eph_branch=true`) per visible eph
        // content layer.  The fused supplement is a no-op (atoms hold the
        // content).  The verb stays layer-agnostic — one `scan`, no shard
        // knowledge — and `sharded_scan` owns the visibility mapping.
        Ok(Some(LayerSpec::sharded_scan(hash, scan)))
    }

    /// Reconstruct the truncation warning every time the layer reports
    /// truncated=true.  Cache hits and misses both reach here because
    /// `layers.truncated` is read on both paths.  The verb owns the
    /// wording and uses its own span, so the warning UX is identical
    /// across calls.
    fn make_truncation_warning(&self) -> Option<crate::diagnostic::Diagnostic> {
        Some(crate::diagnostic::Diagnostic::truncation(
            self.span.clone(),
            format!(
                "search({:?}): result truncated at {} matches in at least one \
                 project; narrow the query (more specific text, \
                 project(\"name\"), whole_word=\"true\")",
                self.query, self.limit,
            ),
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
