use crate::models_diesel::{Object, Project, Symbol, SymbolInstance, SymbolRef};
use crate::symbols::{FileId, Occurrence, SymbolId, SymbolInstanceId, SymbolScope, SymbolType};
use std::hash::{Hash, Hasher};

/// Well-known ephemeral layer ID used as a canary.  If any row with this
/// `layer` appears in a `Selection`, it means the layer filter was
/// bypassed — a data-isolation violation.
pub const CANARY_LAYER_ID: i64 = -999999;

/// A project root layer visible to the current request: the persistent layer
/// id (positive, one per project), the owning project's id, and the root's
/// identity hash.  The layer id feeds visibility binds; the project id scopes
/// per-root populate reads/writes; the hash is folded into base-layer cache
/// hashes so cache identity tracks root identity (and, once roots become
/// version-hashed, content).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootLayer {
    pub id: i64,
    pub project_id: i32,
    pub hash: Vec<u8>,
}

/// The layer set visible to the current request: the root layers (persistent
/// data of the visible projects) plus ONE ephemeral chain PER root — the
/// layers the request has materialised for that root, in materialisation
/// order.
///
/// Roots are fixed at construction ([`EphContext::rooted`] is the only
/// constructor — a context cannot exist without an explicit root set); each
/// root's chain grows via [`EphContext::push`].  Chain topology is linear
/// per root: a root's next layer parents on [`EphContext::chain_last`] of
/// that root, and base layers parent on the root itself.
///
/// Lockstep invariant: every layer-creating statement materialises for every
/// visible root, and chains grow only through [`EphContext::push_round`],
/// which appends one statement's layers for EVERY root atomically — so
/// out-of-lockstep states are unrepresentable and "has a chain" is a
/// request-level property by construction.
///
/// Visibility stays a FLAT set: [`EphContext::visible_ids`] is roots ∪ all
/// chains; queries never see the forest.
///
/// `clone()` is a full copy.  This is intentional: chains are 0-2 elements
/// and root sets a handful in practice, the snapshot semantics at
/// statement-queue time are clearer when each pending future captures its own
/// buffer, and the refcount overhead of an `Arc` wrapper isn't worth it.  Do
/// not "optimise" by wrapping in `Arc` without measuring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EphContext {
    /// Sorted by id, deduped — constructor-enforced so visibility binds and
    /// hash salts are deterministic for a given root set.
    roots: Vec<RootLayer>,
    /// Per-root ephemeral chains, parallel to `roots` — every root always
    /// has a chain slot (a missing entry is unrepresentable), and growth
    /// happens only through [`EphContext::push_round`].
    chains: Vec<Vec<i64>>,
}

impl EphContext {
    /// The only constructor: a context is always rooted in an explicit set of
    /// root layers.  An empty set is legal and means "no persistent data
    /// visible" (unit tests, canary probes).
    pub fn rooted(mut roots: Vec<RootLayer>) -> Self {
        roots.sort_by(|a, b| a.id.cmp(&b.id));
        roots.dedup_by(|a, b| a.id == b.id);
        let chains = vec![Vec::new(); roots.len()];
        Self { roots, chains }
    }

    fn root_index(&self, root_id: i64) -> Option<usize> {
        self.roots.binary_search_by_key(&root_id, |r| r.id).ok()
    }

    /// Append one statement's freshly materialised layers — for EVERY
    /// visible root, in (root, base-then-supplement) order — to the chains.
    /// This is the ONLY growth path, so lockstep is enforced at the source:
    /// a round that misses a root or names an unknown one is a programming
    /// error and panics.
    pub fn push_round(&mut self, layers: &[(i64, i64)]) {
        for r in &self.roots {
            assert!(
                layers.iter().any(|(root_id, _)| *root_id == r.id),
                "push_round is missing layers for root {}",
                r.id
            );
        }
        for (root_id, layer_id) in layers {
            let idx = self
                .root_index(*root_id)
                .unwrap_or_else(|| panic!("push_round onto unknown root {root_id}"));
            self.chains[idx].push(*layer_id);
        }
    }

    /// Most recently materialised ephemeral layer on `root_id`'s chain (the
    /// parent for that root's next supplement).  `None` under an empty
    /// chain — the next base then parents on the root itself.
    pub fn chain_last(&self, root_id: i64) -> Option<i64> {
        self.root_index(root_id)
            .and_then(|idx| self.chains[idx].last().copied())
    }

    /// True if the request has materialised any layers yet.  Chains grow in
    /// lockstep (see [`EphContext::push_round`]), so checking the first
    /// root's chain answers for all of them.
    pub fn has_chain(&self) -> bool {
        self.chains.first().is_some_and(|c| !c.is_empty())
    }

    /// The visible root layers (sorted by id).
    pub fn roots(&self) -> &[RootLayer] {
        &self.roots
    }

    /// Root layer ids (sorted) — the persistent branch of visibility.
    pub fn root_ids(&self) -> Vec<i64> {
        self.roots.iter().map(|r| r.id).collect()
    }

    /// All visible layer ids: roots (sorted), then each root's chain in
    /// root-id order, each in materialisation order.  This is the SQL
    /// binding set for visibility predicates and the reference set for leak
    /// checks — the forest flattened; queries never see the structure.
    pub fn visible_ids(&self) -> Vec<i64> {
        let mut ids = self.root_ids();
        for chain in &self.chains {
            ids.extend_from_slice(chain);
        }
        ids
    }
}

/// Returns `true` if a single layer value represents a leak relative to
/// the visible layer set.  Every row belongs to a layer, so any id outside
/// the visible set — root or ephemeral — is a leak.
pub(crate) fn is_eph_leak(layer: i64, visible_ids: &[i64]) -> bool {
    !visible_ids.contains(&layer)
}

/// Trait for values that can be checked for ephemeral-layer leaks.
/// Implemented by [`Selection`].
pub trait HasEphLeak {
    fn has_eph_leak(&self, eph: &EphContext) -> bool;
}

/// A wrapper proving that an `layer` isolation check has been performed.
///
/// Produced only by [`Checked::new`], which runs `HasEphLeak::has_eph_leak`
/// and bails on a leak.  Callers receiving a `Checked<T>` can be sure no
/// row inside has an `layer` outside the visible `eph` set at
/// construction time.
///
/// Access the underlying value via [`Checked::into_inner`].
pub struct Checked<T>(T);

impl<T: HasEphLeak> Checked<T> {
    /// Construct a `Checked<T>`, verifying isolation against `eph`.
    /// Returns `Err` (and logs the violation) if a leak is detected.
    pub fn new(value: T, eph: &EphContext) -> anyhow::Result<Self> {
        if value.has_eph_leak(eph) {
            tracing::error!(visible_ids = ?eph.visible_ids(), "layer leak detected — aborting request");
            anyhow::bail!("internal error: ephemeral layer isolation violation");
        }
        Ok(Self(value))
    }
}

impl<T> Checked<T> {
    /// Unwrap, taking ownership of the inner value.
    pub fn into_inner(self) -> T {
        self.0
    }
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
        self.nodes
            .iter()
            .map(|node| node.symbol_instance.id)
            .collect()
    }
}

impl HasEphLeak for Selection {
    fn has_eph_leak(&self, eph: &EphContext) -> bool {
        Selection::has_eph_leak(self, eph)
    }
}

impl Selection {
    /// Returns `true` if any row in this selection has an `layer` outside
    /// the visible layer set (roots ∪ chain).  A `true` return means the
    /// layer filter was bypassed and foreign data — another project's
    /// root or a foreign ephemeral layer — leaked into the result.
    ///
    /// Prefer wrapping in [`Checked`] at construction time; callers receiving
    /// a `Checked<Selection>` need not re-check.
    pub fn has_eph_leak(&self, eph: &EphContext) -> bool {
        let visible = eph.visible_ids();
        let eph_ids: &[i64] = &visible;
        for n in &self.nodes {
            if is_eph_leak(n.symbol.layer, eph_ids) || is_eph_leak(n.symbol_instance.layer, eph_ids)
            {
                return true;
            }
        }
        for p in &self.parents {
            if is_eph_leak(p.to_symbol.layer, eph_ids)
                || is_eph_leak(p.to_instance.layer, eph_ids)
                || is_eph_leak(p.from_instance.layer, eph_ids)
                || is_eph_leak(p.symbol_ref.layer, eph_ids)
            {
                return true;
            }
        }
        for c in &self.children {
            if is_eph_leak(c.parent_symbol.layer, eph_ids)
                || is_eph_leak(c.symbol.layer, eph_ids)
                || is_eph_leak(c.symbol_instance.layer, eph_ids)
                || is_eph_leak(c.from_instance.layer, eph_ids)
                || is_eph_leak(c.symbol_ref.layer, eph_ids)
            {
                return true;
            }
        }
        for hp in &self.has_parents {
            if is_eph_leak(hp.child_symbol.layer, eph_ids)
                || is_eph_leak(hp.child_instance.layer, eph_ids)
                || is_eph_leak(hp.parent_symbol.layer, eph_ids)
                || is_eph_leak(hp.parent_instance.layer, eph_ids)
            {
                return true;
            }
        }
        for hc in &self.has_children {
            if is_eph_leak(hc.parent_symbol.layer, eph_ids)
                || is_eph_leak(hc.parent_instance.layer, eph_ids)
                || is_eph_leak(hc.child_symbol.layer, eph_ids)
                || is_eph_leak(hc.child_instance.layer, eph_ids)
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

    /// Root layer id used by the test fixtures below.
    const TEST_ROOT: i64 = 1000001;

    fn test_root_ctx() -> EphContext {
        EphContext::rooted(vec![RootLayer {
            project_id: 1,
            id: TEST_ROOT,
            hash: vec![0xab; 32],
        }])
    }

    fn test_symbol(layer: i64) -> Symbol {
        Symbol {
            id: 1,
            name: "test".into(),
            symbol_path: "test".into(),
            project_id: 1,
            symbol_type: 1,
            symbol_scope: None,
            leaf_name: "test".into(),
            layer,
        }
    }

    fn test_instance(layer: i64) -> SymbolInstance {
        SymbolInstance {
            id: 1,
            symbol: 1,
            object_id: 1,
            offset_range: (Bound::Included(0), Bound::Excluded(1)),
            instance_type: 1,
            layer,
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
            root_layer_id: TEST_ROOT,
        }
    }

    fn selection_with_node(sym_eph: i64, inst_eph: i64) -> Selection {
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
        assert!(!Selection::new().has_eph_leak(&EphContext::rooted(vec![])));
    }

    #[test]
    fn root_rows_no_leak() {
        let s = selection_with_node(TEST_ROOT, TEST_ROOT);
        assert!(!s.has_eph_leak(&test_root_ctx()));
    }

    #[test]
    fn eph_row_in_chain_no_leak() {
        let s = selection_with_node(-1, -1);
        let mut eph = test_root_ctx();
        eph.push_round(&[(TEST_ROOT, -1)]);
        assert!(!s.has_eph_leak(&eph));
    }

    #[test]
    fn eph_row_not_in_chain_is_leak() {
        let s = selection_with_node(-1, -1);
        assert!(s.has_eph_leak(&test_root_ctx()));
    }

    #[test]
    fn foreign_root_is_leak() {
        // A row from another project's root layer is a leak even though it is
        // "persistent" — persistent visibility is an explicit root set now.
        let s = selection_with_node(TEST_ROOT + 1, TEST_ROOT + 1);
        assert!(s.has_eph_leak(&test_root_ctx()));
    }

    #[test]
    fn canary_row_detected() {
        let s = selection_with_node(CANARY_LAYER_ID, CANARY_LAYER_ID);
        let mut eph = test_root_ctx();
        eph.push_round(&[(TEST_ROOT, -1)]);
        assert!(s.has_eph_leak(&eph));
    }

    #[test]
    fn mixed_clean_and_leaked() {
        let mut s = selection_with_node(TEST_ROOT, TEST_ROOT);
        s.nodes.push(SelectionNode {
            symbol: test_symbol(-1),
            symbol_instance: test_instance(-1),
            object: test_object(),
            project: test_project(),
            query_statements: vec![],
        });
        assert!(s.has_eph_leak(&test_root_ctx()));
    }

    #[test]
    fn symbol_leak_only() {
        let s = selection_with_node(-1, TEST_ROOT);
        assert!(s.has_eph_leak(&test_root_ctx()));
    }

    #[test]
    fn instance_leak_only() {
        let s = selection_with_node(TEST_ROOT, -1);
        assert!(s.has_eph_leak(&test_root_ctx()));
    }

    #[test]
    fn roots_sorted_and_deduped() {
        let eph = EphContext::rooted(vec![
            RootLayer {
                project_id: 1,
                id: 7,
                hash: vec![7; 32],
            },
            RootLayer {
                project_id: 1,
                id: 3,
                hash: vec![3; 32],
            },
            RootLayer {
                project_id: 1,
                id: 7,
                hash: vec![7; 32],
            },
        ]);
        assert_eq!(eph.root_ids(), vec![3, 7]);
        assert_eq!(eph.visible_ids(), vec![3, 7]);
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
