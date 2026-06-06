use crate::models_diesel::{Object, Project, Symbol, SymbolInstance, SymbolRef};
use crate::symbols::{
    SymbolInstanceId, FileId, Occurrence, SymbolId, SymbolScope, SymbolType,
};
use std::hash::{Hash, Hasher};

/// Well-known ephemeral layer ID used as a canary.  If any row with this
/// `eph_layer` appears in a `Selection`, it means the eph_layer filter was
/// bypassed — a data-isolation violation.
pub const CANARY_LAYER_ID: i64 = -999999;

/// Returns `true` if a single eph_layer value represents a leak relative to `eph_ids`.
pub(crate) fn is_eph_leak(eph_layer: Option<i64>, eph_ids: &[i64]) -> bool {
    match eph_layer {
        None => false,
        Some(id) => !eph_ids.contains(&id),
    }
}

/// Trait for values that can be checked for ephemeral-layer leaks.
/// Implemented by [`Selection`].
pub trait HasEphLeak {
    fn has_eph_leak(&self, eph_ids: &[i64]) -> bool;
}

/// A wrapper proving that an `eph_layer` isolation check has been performed.
///
/// Produced only by [`Checked::new`], which runs `HasEphLeak::has_eph_leak`
/// and bails on a leak.  Callers receiving a `Checked<T>` can be sure no
/// row inside has an `eph_layer` outside the visible `eph_ids` set at
/// construction time.
///
/// Access the underlying value via [`Checked::into_inner`].
pub struct Checked<T>(T);

impl<T: HasEphLeak> Checked<T> {
    /// Construct a `Checked<T>`, verifying isolation against `eph_ids`.
    /// Returns `Err` (and logs the violation) if a leak is detected.
    pub fn new(value: T, eph_ids: &[i64]) -> anyhow::Result<Self> {
        if value.has_eph_leak(eph_ids) {
            tracing::error!(?eph_ids, "eph_layer leak detected — aborting request");
            anyhow::bail!("internal error: ephemeral layer isolation violation");
        }
        Ok(Self(value))
    }
}

impl<T> Checked<T> {
    /// Unwrap, taking ownership of the inner value.
    pub fn into_inner(self) -> T { self.0 }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ObjectFullDiesel {
    pub id: FileId,
    pub module_path: String,
    pub filesystem_path: String,
    pub filetype: String,
    pub content_hash: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ReferenceFullDiesel {
    pub from_symbol_instance: SymbolInstanceId,
    pub to_symbol: SymbolId,
    pub occurrence: Occurrence,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SymbolInstanceFullDiesel {
    pub id: SymbolInstanceId,
    pub symbol: SymbolId,
    pub name: String,
    pub symbol_scope: SymbolScope,
    pub object: ObjectFullDiesel,
    pub symbol_type: SymbolType,
    pub occurrence: Occurrence,

    pub children: Vec<ReferenceFullDiesel>,
    pub parents: Vec<ReferenceFullDiesel>,
}

/// Provenance info: which query statement produced this node.
#[derive(Debug, Clone)]
pub struct QueryStatementRange {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// A node selected by a query. `query_statements` is metadata excluded from
/// identity (Hash/Eq) so duplicate nodes can be merged while accumulating
/// which statements contributed them.
#[derive(Debug, Clone)]
pub struct SelectionNode {
    pub symbol: Symbol,
    pub symbol_instance: SymbolInstance,
    pub object: Object,
    pub project: Project,
    pub query_statements: Vec<QueryStatementRange>,
}

impl Hash for SelectionNode {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.symbol.hash(state);
        self.symbol_instance.hash(state);
        self.object.hash(state);
        self.project.hash(state);
    }
}

impl PartialEq for SelectionNode {
    fn eq(&self, other: &Self) -> bool {
        self.symbol == other.symbol
            && self.symbol_instance == other.symbol_instance
            && self.object == other.object
            && self.project == other.project
    }
}

impl Eq for SelectionNode {}

#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceResult {
    pub parent_symbol: Symbol,
    pub symbol: Symbol,
    pub symbol_instance: SymbolInstance,
    pub from_instance: SymbolInstance,
    pub symbol_ref: SymbolRef,
    pub from_object: Object,
}

pub type ChildReference = ReferenceResult;

#[derive(Debug, Clone, PartialEq)]
pub struct ParentReference {
    pub to_symbol: Symbol,
    pub to_instance: SymbolInstance,
    pub from_instance: SymbolInstance,
    pub symbol_ref: SymbolRef,
}

/// Containment relationship: parent contains child (parent.offset_range @> child.offset_range)
#[derive(Debug, Clone, PartialEq)]
pub struct HasChildReference {
    pub parent_symbol: Symbol,
    pub parent_instance: SymbolInstance,
    pub child_symbol: Symbol,
    pub child_instance: SymbolInstance,
    pub parent_object: Object,
}

/// Containment relationship: child is contained by parent
#[derive(Debug, Clone, PartialEq)]
pub struct HasParentReference {
    pub child_symbol: Symbol,
    pub child_instance: SymbolInstance,
    pub parent_symbol: Symbol,
    pub parent_instance: SymbolInstance,
}

#[derive(Clone, PartialEq)]
pub struct Selection {
    pub nodes: Vec<SelectionNode>,
    // Reference-based relationships (calls)
    pub parents: Vec<ParentReference>,
    pub children: Vec<ChildReference>,
    // Containment relationships (composition)
    pub has_parents: Vec<HasParentReference>,
    pub has_children: Vec<HasChildReference>,
}

impl Selection {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            parents: Vec::new(),
            children: Vec::new(),
            has_parents: Vec::new(),
            has_children: Vec::new(),
        }
    }

    pub fn extend(&mut self, other: Selection) {
        self.nodes.extend(other.nodes);
        self.parents.extend(other.parents);
        self.children.extend(other.children);
        self.has_parents.extend(other.has_parents);
        self.has_children.extend(other.has_children);
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn get_instance_ids(&self) -> Vec<i64> {
        self.nodes.iter().map(|node| node.symbol_instance.id).collect()
    }
}

impl HasEphLeak for Selection {
    fn has_eph_leak(&self, eph_ids: &[i64]) -> bool {
        Selection::has_eph_leak(self, eph_ids)
    }
}

impl Selection {

    /// Returns `true` if any row in this selection has an `eph_layer` that is
    /// not in `eph_ids`.  A `true` return means the eph_layer filter was
    /// bypassed and foreign ephemeral data leaked into the result.
    ///
    /// Prefer wrapping in [`Checked`] at construction time; callers receiving
    /// a `Checked<Selection>` need not re-check.
    pub fn has_eph_leak(&self, eph_ids: &[i64]) -> bool {
        for n in &self.nodes {
            if is_eph_leak(n.symbol.eph_layer, eph_ids)
                || is_eph_leak(n.symbol_instance.eph_layer, eph_ids)
            {
                return true;
            }
        }
        for p in &self.parents {
            if is_eph_leak(p.to_symbol.eph_layer, eph_ids)
                || is_eph_leak(p.to_instance.eph_layer, eph_ids)
                || is_eph_leak(p.from_instance.eph_layer, eph_ids)
                || is_eph_leak(p.symbol_ref.eph_layer, eph_ids)
            {
                return true;
            }
        }
        for c in &self.children {
            if is_eph_leak(c.parent_symbol.eph_layer, eph_ids)
                || is_eph_leak(c.symbol.eph_layer, eph_ids)
                || is_eph_leak(c.symbol_instance.eph_layer, eph_ids)
                || is_eph_leak(c.from_instance.eph_layer, eph_ids)
                || is_eph_leak(c.symbol_ref.eph_layer, eph_ids)
            {
                return true;
            }
        }
        for hp in &self.has_parents {
            if is_eph_leak(hp.child_symbol.eph_layer, eph_ids)
                || is_eph_leak(hp.child_instance.eph_layer, eph_ids)
                || is_eph_leak(hp.parent_symbol.eph_layer, eph_ids)
                || is_eph_leak(hp.parent_instance.eph_layer, eph_ids)
            {
                return true;
            }
        }
        for hc in &self.has_children {
            if is_eph_leak(hc.parent_symbol.eph_layer, eph_ids)
                || is_eph_leak(hc.parent_instance.eph_layer, eph_ids)
                || is_eph_leak(hc.child_symbol.eph_layer, eph_ids)
                || is_eph_leak(hc.child_instance.eph_layer, eph_ids)
            {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models_diesel::{Object, Project, Symbol, SymbolInstance};
    use std::collections::Bound;

    fn test_symbol(eph_layer: Option<i64>) -> Symbol {
        Symbol {
            id: 1,
            name: "test".into(),
            symbol_path: "test".into(),
            project_id: 1,
            symbol_type: 1,
            symbol_scope: None,
            leaf_name: "test".into(),
            eph_layer,
        }
    }

    fn test_instance(eph_layer: Option<i64>) -> SymbolInstance {
        SymbolInstance {
            id: 1,
            symbol: 1,
            object_id: 1,
            offset_range: (Bound::Included(0), Bound::Excluded(1)),
            instance_type: 1,
            eph_layer,
        }
    }

    fn test_object() -> Object {
        Object {
            id: 1,
            project_id: 1,
            module_path: "".into(),
            filesystem_path: "/test".into(),
            filetype: "c".into(),
            content_hash: "".into(),
        }
    }

    fn test_project() -> Project {
        Project {
            id: 1,
            project_name: "test".into(),
            root_path: "/test".into(),
            upload_status: "complete".into(),
        }
    }

    fn selection_with_node(sym_eph: Option<i64>, inst_eph: Option<i64>) -> Selection {
        let mut s = Selection::new();
        s.nodes.push(SelectionNode {
            symbol: test_symbol(sym_eph),
            symbol_instance: test_instance(inst_eph),
            object: test_object(),
            project: test_project(),
            query_statements: vec![],
        });
        s
    }

    #[test]
    fn empty_selection_no_leak() {
        assert!(!Selection::new().has_eph_leak(&[]));
    }

    #[test]
    fn persistent_rows_no_leak() {
        let s = selection_with_node(None, None);
        assert!(!s.has_eph_leak(&[]));
    }

    #[test]
    fn eph_row_in_eph_ids_no_leak() {
        let s = selection_with_node(Some(-1), Some(-1));
        assert!(!s.has_eph_leak(&[-1]));
    }

    #[test]
    fn eph_row_not_in_eph_ids_is_leak() {
        let s = selection_with_node(Some(-1), Some(-1));
        assert!(s.has_eph_leak(&[]));
    }

    #[test]
    fn canary_row_detected() {
        let s = selection_with_node(Some(CANARY_LAYER_ID), Some(CANARY_LAYER_ID));
        assert!(s.has_eph_leak(&[-1]));
    }

    #[test]
    fn mixed_clean_and_leaked() {
        let mut s = selection_with_node(None, None);
        s.nodes.push(SelectionNode {
            symbol: test_symbol(Some(-1)),
            symbol_instance: test_instance(Some(-1)),
            object: test_object(),
            project: test_project(),
            query_statements: vec![],
        });
        assert!(s.has_eph_leak(&[]));
    }

    #[test]
    fn symbol_leak_only() {
        let s = selection_with_node(Some(-1), None);
        assert!(s.has_eph_leak(&[]));
    }

    #[test]
    fn instance_leak_only() {
        let s = selection_with_node(None, Some(-1));
        assert!(s.has_eph_leak(&[]));
    }
}

impl std::fmt::Debug for Selection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Selection")
            .field(
                "nodes",
                &self
                    .nodes
                    .iter()
                    .map(|n| n.symbol.name.clone())
                    .collect::<Vec<_>>(),
            )
            .field(
                "parents",
                &self
                    .parents
                    .iter()
                    .map(|p| p.from_instance.id)
                    .collect::<Vec<_>>(),
            )
            .field(
                "children",
                &self
                    .children
                    .iter()
                    .map(|c| c.symbol.name.clone())
                    .collect::<Vec<_>>(),
            )
            .field(
                "has_parents",
                &self
                    .has_parents
                    .iter()
                    .map(|p| p.parent_symbol.name.clone())
                    .collect::<Vec<_>>(),
            )
            .field(
                "has_children",
                &self
                    .has_children
                    .iter()
                    .map(|c| c.child_symbol.name.clone())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}
