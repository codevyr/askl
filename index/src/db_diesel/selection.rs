use crate::models_diesel::{Object, Project, Symbol, SymbolInstance, SymbolRef};
use crate::symbols::{FileId, Occurrence, SymbolId, SymbolInstanceId, SymbolScope, SymbolType};
use std::collections::{hash_map::Entry, HashMap, HashSet};
use std::hash::{Hash, Hasher};

/// Well-known ephemeral layer ID used as a canary.  If any row with this
/// `layer` appears in a `Selection`, it means the layer filter was
/// bypassed — a data-isolation violation.
pub const CANARY_LAYER_ID: i64 = -999999;

/// A project root layer visible to the current request: the persistent layer
/// id (positive, one per project), the owning project's id, the project's
/// name, and the root's identity hash.  The layer id feeds visibility binds;
/// the project id scopes per-root populate reads/writes; the hash is folded
/// into root-shard cache keys so cache identity tracks root identity (and,
/// once roots become version-hashed, content).
///
/// `name` is the project name a request narrows by (`project("…")` takes the
/// same string).  It is metadata only: [`root_shard_hash`] folds `hash`, so
/// carrying the name here changes no cache key.
///
/// [`root_shard_hash`]: crate::db_diesel::root_shard_hash
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootLayer {
    pub id: i64,
    pub project_id: i32,
    pub name: String,
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
/// per root: a root's next layer parents on [`EphContext::tip`] of
/// that root, and root shards parent on the root itself.
///
/// Lockstep invariant: every layer-creating command materialises for every
/// visible root, and chains grow only through
/// [`EphContext::push_materialisation`], which appends one top-level
/// statement's (tree's) layers for EVERY root
/// atomically — so out-of-lockstep states are unrepresentable and "has a
/// chain" is a request-level property by construction.
///
/// Visibility stays a FLAT set: [`EphContext::visible_ids`] is roots ∪ all
/// chains; queries never see the forest.
///
/// `clone()` is a full copy.  This is intentional: chains are 0-2 elements
/// and root sets a handful in practice, the snapshot semantics at
/// statement-queue time are clearer when each pending future captures its own
/// buffer, and the refcount overhead of an `Arc` wrapper isn't worth it.  Do
/// not "optimise" by wrapping in `Arc` without measuring.
/// How many distinct result symbols a query ultimately wants (`0` = unlimited).
///
/// The *exact* final cap is applied post-hoc in the API renderer (it must prune
/// whole nodes-with-edges to stay referentially valid).  This value lets a
/// row-producing leaf query push a *bound* into its own SQL so it stops far
/// short of materialising everything the renderer would only throw away — the
/// budget facet of query "fusion".  Carried on [`EphContext`] because it must
/// reach every `find_symbol` leaf, which already receives the eph context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResultBudget(usize);

impl ResultBudget {
    /// No bound — leaf queries run unlimited (the legacy behaviour).
    pub const UNLIMITED: Self = Self(0);

    /// Cap on distinct result symbols; `0` is treated as unlimited.
    pub fn symbols(cap: usize) -> Self {
        Self(cap)
    }

    /// SQL `LIMIT` for a single row-producing leaf, or `None` when unlimited.
    ///
    /// Rows are per-instance but the cap is per-symbol, and the renderer keeps
    /// the top `cap` *distinct symbols*, so we over-fetch by a factor: the leaf
    /// stays bounded while still yielding ≥ `cap` distinct symbols in the common
    /// case.  The over-fetch is a heuristic — tune it against real row counts.
    /// Clamped to `i64::MAX`: a huge user-supplied cap must degrade to
    /// "effectively unlimited", never wrap into a negative SQL LIMIT.
    pub fn leaf_limit(&self) -> Option<i64> {
        const OVERFETCH: usize = 8;
        (self.0 > 0).then(|| self.0.saturating_mul(OVERFETCH).min(i64::MAX as usize) as i64)
    }
}

impl Default for ResultBudget {
    fn default() -> Self {
        Self::UNLIMITED
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EphContext {
    /// Sorted by id, deduped — constructor-enforced so visibility binds and
    /// hash salts are deterministic for a given root set.
    roots: Vec<RootLayer>,
    /// Per-root ephemeral chains, parallel to `roots` — every root always
    /// has a chain slot (a missing entry is unrepresentable), and growth
    /// happens only through [`EphContext::push_materialisation`].
    chains: Vec<Vec<i64>>,
    /// Result budget for this request; default unlimited, set once per request
    /// via [`EphContext::set_result_budget`].  Read by `find_symbol` to bound a
    /// bare selector's neighbourhood queries.
    result_budget: ResultBudget,
}

impl EphContext {
    /// The only constructor: a context is always rooted in an explicit set of
    /// root layers.  An empty set is legal and means "no persistent data
    /// visible" (unit tests, canary probes).
    pub fn rooted(mut roots: Vec<RootLayer>) -> Self {
        roots.sort_by(|a, b| a.id.cmp(&b.id));
        roots.dedup_by(|a, b| a.id == b.id);
        let chains = vec![Vec::new(); roots.len()];
        Self {
            roots,
            chains,
            result_budget: ResultBudget::UNLIMITED,
        }
    }

    /// This request's result budget (see [`ResultBudget`]).
    pub fn result_budget(&self) -> ResultBudget {
        self.result_budget
    }

    /// Set the result budget for this request.  Called once, at the root, from
    /// the API layer; `clone()`/`push_materialisation` carry it forward unchanged.
    pub fn set_result_budget(&mut self, budget: ResultBudget) {
        self.result_budget = budget;
    }

    fn root_index(&self, root_id: i64) -> Option<usize> {
        self.roots.binary_search_by_key(&root_id, |r| r.id).ok()
    }

    /// Append one top-level statement's (tree's) freshly materialised layers
    /// to the chains: every layer-bearing command's contribution in
    /// substatement pre-order, each in its canonical internal order (roots
    /// ascending; root shard → layer shards → selection shard).  A
    /// materialisation contributes one OR MORE layers per root — per command a
    /// root shard, zero or more layer shards, then a selection shard iff the
    /// pre-tree chain was non-empty — so per root the materialisation's LAST
    /// layer becomes the tree's tip.  This is the ONLY growth path, so
    /// lockstep is enforced at the source: a materialisation that misses a
    /// root entirely, or names an unknown one, is a programming error and
    /// panics.
    pub fn push_materialisation(&mut self, layers: &[(i64, i64)]) {
        for r in &self.roots {
            assert!(
                layers.iter().any(|(root_id, _)| *root_id == r.id),
                "push_materialisation is missing layers for root {}",
                r.id
            );
        }
        for (root_id, layer_id) in layers {
            let idx = self
                .root_index(*root_id)
                .unwrap_or_else(|| panic!("push_materialisation onto unknown root {root_id}"));
            self.chains[idx].push(*layer_id);
        }
    }

    /// Most recently materialised ephemeral layer on `root_id`'s chain (the
    /// parent for that root's next selection shard).  `None` under an empty
    /// chain — the next root shard then parents on the root itself.
    pub fn tip(&self, root_id: i64) -> Option<i64> {
        self.root_index(root_id)
            .and_then(|idx| self.chains[idx].last().copied())
    }

    /// True if the request has materialised any layers yet.  Chains grow in
    /// lockstep (see [`EphContext::push_materialisation`]), so checking the first
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
    /// True when any leaf query that produced this selection hit the request's
    /// [`ResultBudget`] LIMIT — the selection may be missing rows.  Consumers
    /// surface this as a truncation warning on the owning command; it must
    /// never be silently dropped.
    pub budget_bounded: bool,
}

impl Selection {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            parents: Vec::new(),
            children: Vec::new(),
            has_parents: Vec::new(),
            has_children: Vec::new(),
            budget_bounded: false,
        }
    }

    /// Set-union merge: `extend` is idempotent, so a selection stored under
    /// several selectors and re-folded (statement-level OR) accumulates no
    /// duplicates.
    ///
    /// Nodes are identified by instance id — the same identity the renderer's
    /// `node_map` uses — and a duplicate node's `query_statements` provenance
    /// is merged span-uniquely onto the surviving node, mirroring the
    /// renderer (which only ever merges distinct statement spans).  Edge rows
    /// collapse only when literally identical: their keys include the ref
    /// identity (id + occurrence offsets), so two distinct call sites between
    /// the same instances remain two rows.
    pub fn extend(&mut self, other: Selection) {
        let mut by_instance: HashMap<i64, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(pos, node)| (node.symbol_instance.id, pos))
            .collect();
        for node in other.nodes {
            match by_instance.entry(node.symbol_instance.id) {
                Entry::Occupied(slot) => {
                    let existing = &mut self.nodes[*slot.get()];
                    for qs in node.query_statements {
                        if !existing
                            .query_statements
                            .iter()
                            .any(|have| have.start == qs.start && have.end == qs.end)
                        {
                            existing.query_statements.push(qs);
                        }
                    }
                }
                Entry::Vacant(slot) => {
                    slot.insert(self.nodes.len());
                    self.nodes.push(node);
                }
            }
        }

        extend_dedup(&mut self.parents, other.parents, |p| {
            (
                p.symbol_ref.id,
                p.symbol_ref.from_offset_range,
                p.from_instance.id,
                p.to_instance.id,
            )
        });
        extend_dedup(&mut self.children, other.children, |c| {
            (
                c.symbol_ref.id,
                c.symbol_ref.from_offset_range,
                c.from_instance.id,
                c.symbol_instance.id,
            )
        });
        extend_dedup(&mut self.has_parents, other.has_parents, |hp| {
            (hp.parent_instance.id, hp.child_instance.id)
        });
        extend_dedup(&mut self.has_children, other.has_children, |hc| {
            (hc.parent_instance.id, hc.child_instance.id)
        });
        self.budget_bounded |= other.budget_bounded;
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

/// Append `src` rows to `dst`, dropping any row whose `key` already occurs
/// in either list.  Keeps first occurrence; order otherwise preserved.
fn extend_dedup<T, K: Hash + Eq>(dst: &mut Vec<T>, src: Vec<T>, key: impl Fn(&T) -> K) {
    let mut seen: HashSet<K> = dst.iter().map(&key).collect();
    dst.extend(src.into_iter().filter(|row| seen.insert(key(row))));
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
            if is_eph_leak(n.symbol.layer, eph_ids)
                || is_eph_leak(n.symbol_instance.layer, eph_ids)
                || is_eph_leak(n.object.layer, eph_ids)
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
                || is_eph_leak(c.from_object.layer, eph_ids)
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
                || is_eph_leak(hc.parent_object.layer, eph_ids)
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
            name: "p1".to_string(),
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
            layer: TEST_ROOT,
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
    fn result_budget_leaf_limit() {
        // Unlimited (0) → no SQL bound.
        assert_eq!(ResultBudget::UNLIMITED.leaf_limit(), None);
        assert_eq!(ResultBudget::symbols(0).leaf_limit(), None);
        // Otherwise cap × over-fetch (8).
        assert_eq!(ResultBudget::symbols(1).leaf_limit(), Some(8));
        assert_eq!(ResultBudget::symbols(100).leaf_limit(), Some(800));
    }

    #[test]
    fn eph_context_carries_budget() {
        let mut eph = EphContext::rooted(vec![]);
        assert_eq!(eph.result_budget(), ResultBudget::UNLIMITED);
        eph.set_result_budget(ResultBudget::symbols(50));
        assert_eq!(eph.result_budget().leaf_limit(), Some(400));
        // clone() carries the budget forward unchanged.
        assert_eq!(eph.clone().result_budget(), ResultBudget::symbols(50));
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
        eph.push_materialisation(&[(TEST_ROOT, -1)]);
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
        eph.push_materialisation(&[(TEST_ROOT, -1)]);
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
    fn object_layer_leak_detected() {
        // Build a node where symbol and instance are in the visible root layer
        // but the object's layer is NOT visible (a negative eph id not in visible_ids).
        // has_eph_leak must return true.
        let mut s = Selection::new();
        s.nodes.push(SelectionNode {
            symbol: test_symbol(TEST_ROOT),
            symbol_instance: test_instance(TEST_ROOT),
            object: Object {
                layer: -42, // not in visible_ids
                ..test_object()
            },
            project: test_project(),
            query_statements: vec![],
        });
        assert!(s.has_eph_leak(&test_root_ctx()));
    }

    #[test]
    fn roots_sorted_and_deduped() {
        let eph = EphContext::rooted(vec![
            RootLayer {
                project_id: 1,
                id: 7,
                name: "p1".to_string(),
                hash: vec![7; 32],
            },
            RootLayer {
                project_id: 1,
                id: 3,
                name: "p1".to_string(),
                hash: vec![3; 32],
            },
            RootLayer {
                project_id: 1,
                id: 7,
                name: "p1".to_string(),
                hash: vec![7; 32],
            },
        ]);
        assert_eq!(eph.root_ids(), vec![3, 7]);
        assert_eq!(eph.visible_ids(), vec![3, 7]);
    }

    // --- Selection::extend set-union semantics -----------------------------

    fn test_instance_id(id: i64) -> SymbolInstance {
        SymbolInstance {
            id,
            ..test_instance(TEST_ROOT)
        }
    }

    fn test_ref(id: i64, lo: i32) -> SymbolRef {
        SymbolRef {
            id,
            to_symbol: 1,
            from_object: 1,
            from_offset_range: (Bound::Included(lo), Bound::Excluded(lo + 1)),
            layer: TEST_ROOT,
        }
    }

    fn node_with(instance_id: i64, span_start: usize, text: &str) -> SelectionNode {
        SelectionNode {
            symbol: test_symbol(TEST_ROOT),
            symbol_instance: test_instance_id(instance_id),
            object: test_object(),
            project: test_project(),
            query_statements: vec![QueryStatementRange {
                start: span_start,
                end: span_start + text.len(),
                text: text.into(),
            }],
        }
    }

    fn parent_ref(ref_id: i64, lo: i32, from: i64, to: i64) -> ParentReference {
        ParentReference {
            to_symbol: test_symbol(TEST_ROOT),
            to_instance: test_instance_id(to),
            from_instance: test_instance_id(from),
            symbol_ref: test_ref(ref_id, lo),
        }
    }

    fn has_child(parent: i64, child: i64) -> HasChildReference {
        HasChildReference {
            parent_symbol: test_symbol(TEST_ROOT),
            parent_instance: test_instance_id(parent),
            child_symbol: test_symbol(TEST_ROOT),
            child_instance: test_instance_id(child),
            parent_object: test_object(),
        }
    }

    #[test]
    fn extend_merges_duplicate_nodes_and_provenance() {
        let mut a = Selection::new();
        a.nodes.push(node_with(7, 0, "\"x\""));
        let mut b = Selection::new();
        b.nodes.push(node_with(7, 10, "\"y\""));
        b.nodes.push(node_with(8, 10, "\"y\""));
        a.extend(b);
        assert_eq!(a.nodes.len(), 2);
        // Duplicate node collapsed; both statements' provenance survives.
        let texts: Vec<_> = a.nodes[0]
            .query_statements
            .iter()
            .map(|q| q.text.as_str())
            .collect();
        assert_eq!(texts, vec!["\"x\"", "\"y\""]);
    }

    #[test]
    fn extend_dedups_identical_edges_keeps_distinct_call_sites() {
        let mut a = Selection::new();
        a.parents.push(parent_ref(11, 100, 1, 2));
        let mut b = Selection::new();
        // Literally identical row: collapses.
        b.parents.push(parent_ref(11, 100, 1, 2));
        // Second call site between the same instances: must survive.
        b.parents.push(parent_ref(12, 200, 1, 2));
        a.extend(b);
        assert_eq!(a.parents.len(), 2);
    }

    #[test]
    fn extend_dedups_containment_by_instance_pair() {
        let mut a = Selection::new();
        a.has_children.push(has_child(1, 2));
        let mut b = Selection::new();
        b.has_children.push(has_child(1, 2));
        b.has_children.push(has_child(1, 3));
        a.extend(b);
        assert_eq!(a.has_children.len(), 2);
    }

    #[test]
    fn extend_is_idempotent() {
        let mut a = Selection::new();
        a.nodes.push(node_with(7, 0, "\"x\""));
        a.parents.push(parent_ref(11, 100, 1, 2));
        a.has_children.push(has_child(1, 2));
        let snapshot = a.clone();
        a.extend(snapshot.clone());
        a.extend(snapshot);
        assert_eq!(a.nodes.len(), 1);
        assert_eq!(a.nodes[0].query_statements.len(), 1);
        assert_eq!(a.parents.len(), 1);
        assert_eq!(a.has_children.len(), 1);
    }

    #[test]
    fn extend_ors_budget_bounded() {
        let mut a = Selection::new();
        let mut b = Selection::new();
        b.budget_bounded = true;
        a.extend(b);
        assert!(a.budget_bounded);
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
