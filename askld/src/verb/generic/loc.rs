use crate::cfg::ControlFlowGraph;
use crate::span::Span;
use anyhow::{bail, Result};
use async_trait::async_trait;
use index::db_diesel::{
    alloc_ephemeral_instance_id, alloc_ephemeral_symbol_id, CompositeFilter, EphemeralOverlay,
    INSTANCE_TYPE_DEFINITION, SYMBOL_TYPE_FUNCTION, ScopeContext, Selection, SymbolInstanceIdMixin,
};
use index::symbols::SymbolInstanceId;
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use super::super::{DeriveMethod, Selector, Verb};

// ============================================================================
// LocSelector — synthetic anchor for a source location.
// ============================================================================

/// Creates an ephemeral symbol+instance anchored to a specific file and line.
///
/// Named args: first positional arg is the file path (suffix match); second is
/// the line number (1-based).  Optional: project (project name filter).
///
/// Semantics:
/// - Looks up objects matching the file path (suffix match against filesystem_path).
/// - For each matching object, reads file contents and computes the byte offset
///   range `[start_of_line_N, start_of_line_{N+1})`.
/// - Allocates ephemeral symbol + instance IDs and injects them into the overlay.
/// - Returns the injected instance via find_symbol + SymbolInstanceIdMixin.
#[derive(Debug)]
pub(in crate::verb) struct LocSelector {
    span: Span,
    file: String,
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
        let file = positional
            .first()
            .ok_or_else(|| anyhow::anyhow!("loc requires a file path as the first argument"))?
            .clone();
        let line_str = positional
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("loc requires a line number as the second argument"))?;
        let line = line_str
            .parse::<usize>()
            .map_err(|_| anyhow::anyhow!("loc: line number must be a positive integer"))?;
        if line == 0 {
            bail!("loc: line number must be >= 1");
        }
        let project = named.get("project").cloned();

        Ok(Arc::new(Self {
            span,
            file,
            line,
            project,
        }))
    }
}

impl Verb for LocSelector {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }
    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }
    fn as_selector<'a>(&'a self) -> anyhow::Result<&'a dyn super::super::Selector> {
        Ok(self)
    }
}

impl Display for LocSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LocSelector({}:{})", self.file, self.line)
    }
}

#[async_trait(?Send)]
impl Selector for LocSelector {
    async fn select_from_all_impl(
        &self,
        cfg: &ControlFlowGraph,
        _filter: CompositeFilter,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
    ) -> Result<(Option<Selection>, EphemeralOverlay)> {
        let project_name = self.project.as_deref();
        let objects = cfg
            .index
            .find_objects_by_path(&self.file, project_name)
            .await?;

        if objects.is_empty() {
            return Ok((Some(Selection::new()), EphemeralOverlay::empty()));
        }

        let mut overlay = EphemeralOverlay::empty();
        let mut instance_ids: Vec<SymbolInstanceId> = Vec::new();

        for (file_id, project_id) in objects {
            let content = match cfg.index.get_file_contents(file_id).await {
                Ok(c) => c,
                Err(_) => continue,
            };
            let bytes = content.as_bytes();

            let start = match index::symbols::offset_from_line_col(bytes, self.line, 1) {
                Some(s) => s,
                None => continue, // line out of range
            };
            let end = index::symbols::offset_from_line_col(bytes, self.line + 1, 1)
                .unwrap_or_else(|| i32::try_from(bytes.len()).unwrap_or(i32::MAX));

            let sym_id = alloc_ephemeral_symbol_id();
            let inst_id = alloc_ephemeral_instance_id();
            let project_id_i32: i32 = project_id.into();
            let file_id_i32: i32 = file_id.into();

            overlay.symbols.push(
                sym_id, String::new(), format!("eph_loc_{}", inst_id),
                project_id_i32, SYMBOL_TYPE_FUNCTION, None, String::new(),
            );
            overlay.instances.push(
                inst_id, sym_id, file_id_i32, start, end, INSTANCE_TYPE_DEFINITION,
            );

            instance_ids.push(SymbolInstanceId::new(inst_id));
        }

        if instance_ids.is_empty() {
            return Ok((Some(Selection::new()), overlay));
        }

        let filter = CompositeFilter::leaf(SymbolInstanceIdMixin::new(&instance_ids));
        let selection = cfg
            .index
            .find_symbol(&filter, parent_scope, children_scope, &overlay)
            .await?;

        Ok((Some(selection), overlay))
    }
}
