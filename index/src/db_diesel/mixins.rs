use std::marker::PhantomData;

use diesel::dsl::Eq;
use diesel::expression::{is_aggregate, BoxableExpression, ValidGrouping};
use diesel::helper_types::{AsSelect, InnerJoinQuerySource};
use diesel::internal::table_macro::{BoxedSelectStatement, FromClause};
use diesel::pg::Pg;
use diesel::prelude::*;
use diesel::query_builder::{AstPass, QueryFragment, QueryId};
use diesel::query_source::{Alias, AliasedField};
use diesel::sql_types::{BigInt, Bool, Int4range, Integer, Nullable, Text};
use sha2::{Digest, Sha256};

use crate::db_diesel::selection::EphContext;
use crate::ltree::Ltree;
use crate::models_diesel::{Object, Project, Symbol, SymbolInstance, SymbolRef};
use crate::schema_diesel as index_schema;
use crate::symbols::{
    build_lquery, normalize_leaf_fragment, normalize_symbol_tokens, smart_case_sensitive,
    symbol_name_to_path, SymbolInstanceId,
};

diesel::alias! {
    pub const PARENT_SYMBOLS_ALIAS: Alias<ParentSymbolsAlias> =
        index_schema::symbols as parent_symbols;
    pub const PARENT_DECLS_ALIAS: Alias<ParentDeclsAlias> =
        index_schema::symbol_instances as parent_decls;
    // Aliases for containment queries
    pub const CONTAINER_INSTANCE_ALIAS: Alias<ContainerInstanceAlias> =
        index_schema::symbol_instances as container_instances;
    pub const CONTAINER_SYMBOL_ALIAS: Alias<ContainerSymbolAlias> =
        index_schema::symbols as container_symbols;
    pub const CONTAINER_TYPE_ALIAS: Alias<ContainerTypeAlias> =
        index_schema::symbol_types as container_types;
    pub const CONTAINED_INSTANCE_ALIAS: Alias<ContainedInstanceAlias> =
        index_schema::symbol_instances as contained_instances;
    pub const CONTAINED_SYMBOL_ALIAS: Alias<ContainedSymbolAlias> =
        index_schema::symbols as contained_symbols;
    pub const CONTAINED_TYPE_ALIAS: Alias<ContainedTypeAlias> =
        index_schema::symbol_types as contained_types;
}

type SymbolInstanceJoinSource = InnerJoinQuerySource<
    index_schema::symbols::table,
    index_schema::symbol_instances::table,
    Eq<index_schema::symbols::columns::id, index_schema::symbol_instances::columns::symbol>,
>;

type SymbolInstanceProjectJoinSource = InnerJoinQuerySource<
    SymbolInstanceJoinSource,
    index_schema::projects::table,
    Eq<index_schema::symbols::columns::project_id, index_schema::projects::columns::id>,
>;

type SymbolInstanceProjectObjectJoin = InnerJoinQuerySource<
    SymbolInstanceProjectJoinSource,
    index_schema::objects::table,
    Eq<index_schema::objects::columns::id, index_schema::symbol_instances::columns::object_id>,
>;

type SelectionTuple = (
    AsSelect<Symbol, Pg>,
    AsSelect<SymbolInstance, Pg>,
    AsSelect<Object, Pg>,
    AsSelect<Project, Pg>,
);

pub type CurrentQuery<'a> =
    BoxedSelectStatement<'a, SelectionTuple, FromClause<SymbolInstanceProjectObjectJoin>, Pg>;

type SymbolInstanceColumnsSqlType = (BigInt, BigInt, Integer, Int4range, Integer, BigInt);

type SymbolColumnsSqlType = (
    BigInt,
    Text,
    Ltree,
    Integer,
    Integer,
    Nullable<Integer>,
    Text,
    BigInt,
); // (id, name, symbol_path, project_id, symbol_type, symbol_scope, leaf_name, layer)

type ParentSelectionTuple = (
    AsSelect<SymbolRef, Pg>,
    AsSelect<Symbol, Pg>,
    AsSelect<SymbolInstance, Pg>,
    SymbolInstanceColumnsSqlType, // We cannot use AsSelect<SymbolInstance, Pg> here due to ambiguity
);

type SymbolRefSymbolJoin = InnerJoinQuerySource<
    index_schema::symbol_refs::table,
    index_schema::symbols::table,
    Eq<index_schema::symbol_refs::columns::to_symbol, index_schema::symbols::columns::id>,
>;

type SymbolRefSymbolInstanceJoin = InnerJoinQuerySource<
    SymbolRefSymbolJoin,
    index_schema::symbol_instances::table,
    Eq<index_schema::symbols::columns::id, index_schema::symbol_instances::columns::symbol>,
>;

type ParentDeclOn = Eq<
    AliasedField<ParentDeclsAlias, index_schema::symbol_instances::columns::object_id>,
    index_schema::symbol_refs::columns::from_object,
>;

pub type ParentsQuery<'a> = BoxedSelectStatement<
    'a,
    ParentSelectionTuple,
    FromClause<SymbolRefSymbolInstanceParentInstanceParentSymbolJoin>,
    Pg,
>;

type ChildSelectionTuple = (
    SymbolColumnsSqlType,
    AsSelect<Symbol, Pg>,
    AsSelect<SymbolInstance, Pg>,
    SymbolInstanceColumnsSqlType,
    AsSelect<SymbolRef, Pg>,
    AsSelect<Object, Pg>,
);

type ParentSymbolOn = Eq<
    AliasedField<ParentSymbolsAlias, index_schema::symbols::columns::id>,
    AliasedField<ParentDeclsAlias, index_schema::symbol_instances::columns::symbol>,
>;

type ParentObjectOn = Eq<
    index_schema::objects::columns::id,
    AliasedField<ParentDeclsAlias, index_schema::symbol_instances::columns::object_id>,
>;

type SymbolRefSymbolInstanceParentInstanceJoin =
    InnerJoinQuerySource<SymbolRefSymbolInstanceJoin, Alias<ParentDeclsAlias>, ParentDeclOn>;

type SymbolRefSymbolInstanceParentInstanceParentSymbolJoin = InnerJoinQuerySource<
    SymbolRefSymbolInstanceParentInstanceJoin,
    Alias<ParentSymbolsAlias>,
    ParentSymbolOn,
>;

type SymbolRefSymbolInstanceParentInstanceParentSymbolObjectJoin = InnerJoinQuerySource<
    SymbolRefSymbolInstanceParentInstanceParentSymbolJoin,
    index_schema::objects::table,
    ParentObjectOn,
>;

pub type ChildrenQuery<'a> = BoxedSelectStatement<
    'a,
    ChildSelectionTuple,
    FromClause<SymbolRefSymbolInstanceParentInstanceParentSymbolObjectJoin>,
    Pg,
>;

// ============================================================================
// Containment query types (has_parents, has_children)
// ============================================================================

// has_parents: find containers of current symbols
// Query structure: symbol_instances -> symbols -> symbol_types -> container_instances -> container_symbols -> container_types
type HasParentsSelectionTuple = (
    AsSelect<Symbol, Pg>,         // child_symbol (current)
    AsSelect<SymbolInstance, Pg>, // child_instance (current)
    SymbolColumnsSqlType,         // parent_symbol (container)
    SymbolInstanceColumnsSqlType, // parent_instance (container)
);

// Join type for symbol_instances -> symbols
type InstanceSymbolJoin = InnerJoinQuerySource<
    index_schema::symbol_instances::table,
    index_schema::symbols::table,
    Eq<index_schema::symbol_instances::columns::symbol, index_schema::symbols::columns::id>,
>;

// Join type for symbol_instances -> symbols -> symbol_types
type InstanceSymbolTypeJoin = InnerJoinQuerySource<
    InstanceSymbolJoin,
    index_schema::symbol_types::table,
    Eq<index_schema::symbols::columns::symbol_type, index_schema::symbol_types::columns::id>,
>;

// Join type for ... -> container_instances
type ContainerInstanceOn = Eq<
    AliasedField<ContainerInstanceAlias, index_schema::symbol_instances::columns::object_id>,
    index_schema::symbol_instances::columns::object_id,
>;

type InstanceSymbolTypeContainerInstanceJoin = InnerJoinQuerySource<
    InstanceSymbolTypeJoin,
    Alias<ContainerInstanceAlias>,
    ContainerInstanceOn,
>;

// Join type for ... -> container_symbols
type ContainerSymbolOn = Eq<
    AliasedField<ContainerSymbolAlias, index_schema::symbols::columns::id>,
    AliasedField<ContainerInstanceAlias, index_schema::symbol_instances::columns::symbol>,
>;

type InstanceSymbolTypeContainerInstanceSymbolJoin = InnerJoinQuerySource<
    InstanceSymbolTypeContainerInstanceJoin,
    Alias<ContainerSymbolAlias>,
    ContainerSymbolOn,
>;

// Join type for ... -> container_types
type ContainerTypeOn = Eq<
    AliasedField<ContainerTypeAlias, index_schema::symbol_types::columns::id>,
    AliasedField<ContainerSymbolAlias, index_schema::symbols::columns::symbol_type>,
>;

type HasParentsJoinSource = InnerJoinQuerySource<
    InstanceSymbolTypeContainerInstanceSymbolJoin,
    Alias<ContainerTypeAlias>,
    ContainerTypeOn,
>;

pub type HasParentsQuery<'a> =
    BoxedSelectStatement<'a, HasParentsSelectionTuple, FromClause<HasParentsJoinSource>, Pg>;

// has_children: find symbols contained by current symbols
// Query structure: symbol_instances -> symbols -> symbol_types -> objects -> contained_instances -> contained_symbols -> contained_types
type HasChildrenSelectionTuple = (
    AsSelect<Symbol, Pg>,         // parent_symbol (current)
    AsSelect<SymbolInstance, Pg>, // parent_instance (current)
    SymbolColumnsSqlType,         // child_symbol (contained)
    SymbolInstanceColumnsSqlType, // child_instance (contained)
    AsSelect<Object, Pg>,         // parent_object
);

// Join type for ... -> objects
type InstanceSymbolTypeObjectJoin = InnerJoinQuerySource<
    InstanceSymbolTypeJoin,
    index_schema::objects::table,
    Eq<index_schema::objects::columns::id, index_schema::symbol_instances::columns::object_id>,
>;

// Join type for ... -> contained_instances
type ContainedInstanceOn = Eq<
    AliasedField<ContainedInstanceAlias, index_schema::symbol_instances::columns::object_id>,
    index_schema::symbol_instances::columns::object_id,
>;

type InstanceSymbolTypeObjectContainedInstanceJoin = InnerJoinQuerySource<
    InstanceSymbolTypeObjectJoin,
    Alias<ContainedInstanceAlias>,
    ContainedInstanceOn,
>;

// Join type for ... -> contained_symbols
type ContainedSymbolOn = Eq<
    AliasedField<ContainedSymbolAlias, index_schema::symbols::columns::id>,
    AliasedField<ContainedInstanceAlias, index_schema::symbol_instances::columns::symbol>,
>;

type InstanceSymbolTypeObjectContainedInstanceSymbolJoin = InnerJoinQuerySource<
    InstanceSymbolTypeObjectContainedInstanceJoin,
    Alias<ContainedSymbolAlias>,
    ContainedSymbolOn,
>;

// Join type for ... -> contained_types
type ContainedTypeOn = Eq<
    AliasedField<ContainedTypeAlias, index_schema::symbol_types::columns::id>,
    AliasedField<ContainedSymbolAlias, index_schema::symbols::columns::symbol_type>,
>;

type HasChildrenJoinSource = InnerJoinQuerySource<
    InstanceSymbolTypeObjectContainedInstanceSymbolJoin,
    Alias<ContainedTypeAlias>,
    ContainedTypeOn,
>;

pub type HasChildrenQuery<'a> =
    BoxedSelectStatement<'a, HasChildrenSelectionTuple, FromClause<HasChildrenJoinSource>, Pg>;

fn ltree_filter_sql(column: &str, lquery: &str) -> String {
    format!("{} ~ '{}'::lquery", column, lquery)
}

// ============================================================================
// OwnedSql — an owned raw SQL expression for use in boxed trait objects
// ============================================================================

/// Owned SQL literal expression. Unlike `diesel::dsl::sql()` which borrows,
/// this owns its string and can be boxed as `'static` trait object.
/// Used for raw SQL predicates (ltree queries, NOT EXISTS subqueries).
#[derive(Debug, Clone)]
pub(crate) struct OwnedSql<ST> {
    sql: String,
    _marker: PhantomData<ST>,
}

impl<ST> OwnedSql<ST> {
    pub(crate) fn new(sql: String) -> Self {
        Self {
            sql,
            _marker: PhantomData,
        }
    }
}

impl<ST: 'static + Send + diesel::sql_types::SingleValue> Expression for OwnedSql<ST> {
    type SqlType = ST;
}

impl<ST: 'static + Send> QueryFragment<Pg> for OwnedSql<ST> {
    fn walk_ast<'b>(&'b self, mut pass: AstPass<'_, 'b, Pg>) -> diesel::QueryResult<()> {
        pass.push_sql(&self.sql);
        Ok(())
    }
}

impl<ST: 'static + Send + diesel::sql_types::SingleValue, QS> SelectableExpression<QS>
    for OwnedSql<ST>
{
}
impl<ST: 'static + Send + diesel::sql_types::SingleValue, QS> AppearsOnTable<QS> for OwnedSql<ST> {}

impl<ST> QueryId for OwnedSql<ST> {
    type QueryId = ();
    const HAS_STATIC_QUERY_ID: bool = false;
}

impl<ST: 'static + Send + diesel::sql_types::SingleValue, GB> ValidGrouping<GB> for OwnedSql<ST> {
    type IsAggregate = is_aggregate::No;
}

// ============================================================================
// EphSqlFragment — composable SQL pieces interleaved with i64 array binds
// ============================================================================

/// One piece of an [`EphSqlFragment`].  The fragment serialises by emitting
/// each part in order: `Sql` text is pushed as-is, `BindI64Array` is sent as
/// a single `Array<BigInt>` bind parameter — Diesel assigns positional
/// `$N` placeholders.
#[derive(Debug, Clone)]
enum EphSqlPart {
    Sql(String),
    BindI64Array(Vec<i64>),
}

/// SQL builder that allows interleaving `push_sql` with one or more
/// `Array<BigInt>` bind parameters.  Replaces the older pattern of inlining
/// `ANY(ARRAY[1,2,3]::bigint[])` into raw SQL text, which produced a fresh
/// prepared-statement entry per distinct id vector.  Using bind parameters
/// keeps the statement text constant, so the PostgreSQL plan cache (and
/// `pg_stat_statements`) sees one query shape per logical query.
///
/// Usage (inside a [`FilterLeaf`] implementation):
/// ```rust,ignore
/// EphSqlFragment::<Bool>::builder()
///     .sql("NOT EXISTS (SELECT 1 FROM index.symbol_instances op \
///                       WHERE op.id = ANY(")
///     .bind(self.parent_ids.clone())
///     .sql(") AND ")
///     .eph_visibility("op.layer", vis)
///     .sql(")")
///     .build()
/// ```
#[derive(Debug, Clone)]
pub(crate) struct EphSqlFragment<ST> {
    parts: Vec<EphSqlPart>,
    _marker: PhantomData<ST>,
}

pub(crate) struct EphSqlBuilder<ST> {
    parts: Vec<EphSqlPart>,
    _marker: PhantomData<ST>,
}

impl<ST> EphSqlFragment<ST> {
    pub(crate) fn builder() -> EphSqlBuilder<ST> {
        EphSqlBuilder {
            parts: Vec::new(),
            _marker: PhantomData,
        }
    }
}

impl<ST> EphSqlBuilder<ST> {
    /// Append a literal SQL fragment.
    pub(crate) fn sql(mut self, s: impl Into<String>) -> Self {
        match self.parts.last_mut() {
            Some(EphSqlPart::Sql(buf)) => buf.push_str(&s.into()),
            _ => self.parts.push(EphSqlPart::Sql(s.into())),
        }
        self
    }

    /// Bind an `Array<BigInt>` parameter at this position in the SQL.
    pub(crate) fn bind(mut self, ids: Vec<i64>) -> Self {
        self.parts.push(EphSqlPart::BindI64Array(ids));
        self
    }

    /// Emit the eph-visibility predicate for `column` according to the
    /// token's branch (see [`EphVisibility`]).  This is the SUBQUERY
    /// position: the column is deliberately NOT recorded for the
    /// eph-branch disjointness guard — subquery columns affect row
    /// *membership*, not which selected rows are eph-derived.
    ///
    /// One uniform shape for every branch: `column = ANY($visible)`.  Every
    /// row belongs to a layer, so visibility is always membership in an
    /// explicit set — roots only, or roots ∪ chain.
    pub(crate) fn eph_visibility(mut self, column: &str, vis: &EphVisibility) -> Self {
        let visible = vis.branch.visible();
        let sql = format!("{} = ANY(", column);
        match self.parts.last_mut() {
            Some(EphSqlPart::Sql(buf)) => buf.push_str(&sql),
            _ => self.parts.push(EphSqlPart::Sql(sql)),
        }
        self.parts.push(EphSqlPart::BindI64Array(visible.to_vec()));
        self.parts.push(EphSqlPart::Sql(")".to_string()));
        self
    }

    pub(crate) fn build(self) -> EphSqlFragment<ST> {
        EphSqlFragment {
            parts: self.parts,
            _marker: PhantomData,
        }
    }
}

// ============================================================================
// EphVisibility — the capability token that enforces query partitioning
// ============================================================================

/// Capability token for eph-layer visibility in read queries.
///
/// Query builders can only OBTAIN one from `Index::cached_load_partitioned`
/// (or the Combined constructor used during migration) — the constructors
/// are `pub(crate)` and the chain ids live inside the token, so a builder
/// cannot embed the chain into a persistent-branch query, cannot skip the
/// partition split, and (because keys are rendered from the final SQL)
/// cannot under-key a cache entry.  What remains conventional: eph-branch
/// builders should end with `.filter(vis.guard())`; omitting it is benign
/// for correctness (the branch merge dedups by row identity) but wastes
/// cache bytes — `guard_was_taken` backs a debug assertion in the loader.
#[derive(Debug)]
pub struct EphVisibility {
    branch: VisibilitySpec,
    /// Selected-row eph columns this token was applied to via [`Self::pred`],
    /// in application order — the members of the eph-branch guard.
    /// Interior mutability is single-threaded on purpose (RefCell/Cell):
    /// the token is created, threaded through a synchronous build closure,
    /// and consumed within one loader call — it never crosses threads.
    applied: std::cell::RefCell<Vec<String>>,
    guard_taken: std::cell::Cell<bool>,
}

impl EphVisibility {
    /// Root-only visibility: the persistent branch — rows of the visible
    /// projects' root layers, no ephemeral chain.
    pub(crate) fn root_only(eph: &EphContext) -> Self {
        Self::new(VisibilitySpec::RootOnly(eph.root_ids()))
    }

    pub(crate) fn eph_touching(eph: &EphContext) -> Self {
        Self::new(VisibilitySpec::EphTouching(eph.visible_ids()))
    }

    pub(crate) fn combined(eph: &EphContext) -> Self {
        Self::new(VisibilitySpec::Combined(eph.visible_ids()))
    }

    fn new(branch: VisibilitySpec) -> Self {
        Self {
            branch,
            applied: std::cell::RefCell::new(Vec::new()),
            guard_taken: std::cell::Cell::new(false),
        }
    }

    /// Visibility predicate for a SELECTED-ROW eph column (fully qualified
    /// SQL name, e.g. `"index"."symbols"."layer"` or an alias like
    /// `"parent_decls"."layer"`).  Records the column as a guard member.
    /// The returned fragment applies to any Diesel query source (blanket
    /// `AppearsOnTable`), so DSL and raw-SQL builders use the same call.
    pub(crate) fn pred(&self, column: &str) -> EphSqlFragment<Bool> {
        self.applied.borrow_mut().push(column.to_string());
        EphSqlFragment::<Bool>::builder()
            .eph_visibility(column, self)
            .build()
    }

    /// Visibility predicate for a SUBQUERY-position eph column: rendered the
    /// same way as [`Self::pred`] but NOT recorded as a guard member —
    /// subquery columns affect row membership, not whether a selected row is
    /// eph-derived.
    pub(crate) fn subquery_pred(&self, column: &str) -> EphSqlFragment<Bool> {
        EphSqlFragment::<Bool>::builder()
            .eph_visibility(column, self)
            .build()
    }

    /// Eph-branch disjointness guard: `NOT (c1 > 0 AND c2 > 0 ...)` over the
    /// columns recorded by [`Self::pred`] — a row every one of whose eph
    /// columns is positive already belongs to the persistent branch.
    ///
    /// The sign form is equivalent to root-set membership for guarded
    /// columns: each is independently constrained to the visible set by its
    /// own `pred()` conjunct, and within the visible set the positive ids
    /// are exactly the roots (root ids are structurally positive, ephemeral
    /// ids structurally negative — sequence direction plus the data tables'
    /// `(id > 0) = (layer > 0)` CHECK).  Bind-free on purpose: the guard
    /// SQL is independent of the root set, so cache keys don't grow with
    /// project count.  Returns a no-op `TRUE` fragment for the other
    /// branches so builders can apply it unconditionally.
    pub(crate) fn guard(&self) -> EphSqlFragment<Bool> {
        self.guard_taken.set(true);
        match &self.branch {
            VisibilitySpec::EphTouching(_) => {
                let applied = self.applied.borrow();
                if applied.is_empty() {
                    return EphSqlFragment::<Bool>::builder().sql("TRUE").build();
                }
                let clauses: Vec<String> = applied.iter().map(|c| format!("{} > 0", c)).collect();
                EphSqlFragment::<Bool>::builder()
                    .sql(format!("NOT ({})", clauses.join(" AND ")))
                    .build()
            }
            _ => EphSqlFragment::<Bool>::builder().sql("TRUE").build(),
        }
    }

    pub(crate) fn guard_was_taken(&self) -> bool {
        self.guard_taken.get()
    }

    /// Cloneable snapshot for custom `QueryFragment` impls (the CTE
    /// wrappers) that render visibility inside `walk_ast`.  Taking a
    /// snapshot TRANSFERS guard responsibility: the consuming `walk_ast`
    /// must render the eph-branch disjointness guard itself (see
    /// `CteFindEdgesBetween`), so the loader's guard assertion is
    /// satisfied here.
    pub(crate) fn snapshot(&self) -> VisibilitySpec {
        self.guard_taken.set(true);
        self.branch.clone()
    }
}

/// Owned, cloneable form of a token's branch for CTE `walk_ast` rendering.
/// Only obtainable from a token, so it inherits the capability property.
#[derive(Debug, Clone)]
pub enum VisibilitySpec {
    /// Persistent branch: the visible root layers only.
    RootOnly(Vec<i64>),
    /// Eph branch: roots ∪ chain visible.  The disjointness guard needs no
    /// separate root set — within the visible set, roots are exactly the
    /// positive ids (see [`EphVisibility::guard`]).
    EphTouching(Vec<i64>),
    /// Legacy-visibility mode: roots ∪ chain, no guard.
    Combined(Vec<i64>),
}

impl VisibilitySpec {
    /// The layer-id set a visibility predicate binds for this branch.
    pub(crate) fn visible(&self) -> &[i64] {
        match self {
            VisibilitySpec::RootOnly(roots) => roots,
            VisibilitySpec::EphTouching(visible) => visible,
            VisibilitySpec::Combined(visible) => visible,
        }
    }
}

impl<ST: 'static + Send + diesel::sql_types::SingleValue> Expression for EphSqlFragment<ST> {
    type SqlType = ST;
}

impl<ST: 'static + Send> QueryFragment<Pg> for EphSqlFragment<ST> {
    fn walk_ast<'b>(&'b self, mut pass: AstPass<'_, 'b, Pg>) -> diesel::QueryResult<()> {
        for part in &self.parts {
            match part {
                EphSqlPart::Sql(s) => pass.push_sql(s),
                EphSqlPart::BindI64Array(arr) => {
                    pass.push_bind_param::<diesel::sql_types::Array<BigInt>, _>(arr)?;
                }
            }
        }
        Ok(())
    }
}

impl<ST: 'static + Send + diesel::sql_types::SingleValue, QS> SelectableExpression<QS>
    for EphSqlFragment<ST>
{
}
impl<ST: 'static + Send + diesel::sql_types::SingleValue, QS> AppearsOnTable<QS>
    for EphSqlFragment<ST>
{
}

impl<ST> QueryId for EphSqlFragment<ST> {
    type QueryId = ();
    const HAS_STATIC_QUERY_ID: bool = false;
}

impl<ST: 'static + Send + diesel::sql_types::SingleValue, GB> ValidGrouping<GB>
    for EphSqlFragment<ST>
{
    type IsAggregate = is_aggregate::No;
}

// ============================================================================
// Bool expression type aliases — one per query context
// ============================================================================

type CurrentQS = SymbolInstanceProjectObjectJoin;
type ParentsQS = SymbolRefSymbolInstanceParentInstanceParentSymbolJoin;
type ChildrenQS = SymbolRefSymbolInstanceParentInstanceParentSymbolObjectJoin;
type HasParentsQS = HasParentsJoinSource;
type HasChildrenQS = HasChildrenJoinSource;

pub type CurrentBoolExpr = Box<dyn BoxableExpression<CurrentQS, Pg, SqlType = Bool>>;
pub type ParentsBoolExpr = Box<dyn BoxableExpression<ParentsQS, Pg, SqlType = Bool>>;
pub type ChildrenBoolExpr = Box<dyn BoxableExpression<ChildrenQS, Pg, SqlType = Bool>>;
pub type HasParentsBoolExpr = Box<dyn BoxableExpression<HasParentsQS, Pg, SqlType = Bool>>;
pub type HasChildrenBoolExpr = Box<dyn BoxableExpression<HasChildrenQS, Pg, SqlType = Bool>>;
/// Object-level predicates (used by `search()` to scope its content scan).  Most
/// filters do not constrain at the object layer (they only narrow symbols/instances)
/// and so return `None`.  `ProjectFilterMixin` is the headline implementor today —
/// it restricts to objects in a given project.
pub type ObjectsBoolExpr =
    Box<dyn BoxableExpression<index_schema::objects::table, Pg, SqlType = Bool>>;

// ============================================================================
// FilterLeaf trait and CompositeFilter
// ============================================================================

/// Helper trait enabling `Clone` for `Box<dyn FilterLeaf>`.
/// Automatically implemented for all `FilterLeaf + Clone` types.
pub trait FilterLeafClone {
    fn clone_box(&self) -> Box<dyn FilterLeaf>;
}

impl<T: 'static + FilterLeaf + Clone> FilterLeafClone for T {
    fn clone_box(&self) -> Box<dyn FilterLeaf> {
        Box::new(self.clone())
    }
}

impl Clone for Box<dyn FilterLeaf> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// A leaf filter that produces Diesel boolean expressions for each query context.
/// Each method returns `None` if this leaf does not constrain that query context.
///
/// The eph-visible contexts receive the [`EphVisibility`] token: leaves that
/// need chain visibility inside their SQL (subqueries) MUST render it
/// through the token (they no longer hold an `EphContext` of their own), so
/// a persistent-branch instantiation cannot accidentally embed the chain.
pub trait FilterLeaf: std::fmt::Debug + FilterLeafClone + Send + Sync {
    fn current_expr(&self, _vis: &EphVisibility) -> Option<CurrentBoolExpr> {
        None
    }
    fn parents_expr(&self, _vis: &EphVisibility) -> Option<ParentsBoolExpr> {
        None
    }
    fn children_expr(&self, _vis: &EphVisibility) -> Option<ChildrenBoolExpr> {
        None
    }
    fn has_parents_expr(&self, _vis: &EphVisibility) -> Option<HasParentsBoolExpr> {
        None
    }
    fn has_children_expr(&self, _vis: &EphVisibility) -> Option<HasChildrenBoolExpr> {
        None
    }

    /// Whether `has_children_expr` returns Some for this leaf.  MUST stay
    /// consistent with the expr method and MUST NOT depend on the
    /// visibility token (see `CompositeFilter::constrains_has_children`).
    fn constrains_has_children(&self) -> bool {
        false
    }

    /// Whether this leaf's SQL references chain-visible rows beyond the
    /// selected-row visibility columns (e.g. inside NOT EXISTS / IN
    /// subqueries).  For such filters a persistent row's *membership* can
    /// depend on eph rows, so the persistent/eph partition union does not
    /// equal the legacy single-query result — the loader must fall back to
    /// one Combined query (still cached, chain-keyed).
    fn is_chain_dependent(&self) -> bool {
        false
    }

    /// Object-level predicate, used by `search()` to scope its content scan.
    /// Returns `None` for filters that only constrain at the symbol/instance layer.
    ///
    /// **Contract — implementations MUST NOT vary with `EphContext`.**  The
    /// partitioned cache's base layers (search, loc) are keyed WITHOUT the
    /// ephemeral chain, on the assumption that everything their populate
    /// SQL consumes — which for `search()` is exactly this expression — is
    /// chain-independent.  An eph-sensitive `objects_expr` would change the
    /// base's content while its key stays constant: the first chain to
    /// populate the base would poison it for every other chain.  If a
    /// filter genuinely needs eph-scoped object visibility, it must NOT go
    /// through this hook — it belongs in the supplement half (whose key
    /// folds the chain and the verb's `supplement_extra`).  The regression
    /// test `search_base_reused_with_filters_across_eph_change` fences this.
    fn objects_expr(&self) -> Option<ObjectsBoolExpr> {
        None
    }

    /// Canonical hash of this leaf's semantic state for cache-key composition.
    ///
    /// Used by verbs whose ephemeral layer is parameterised on the surrounding
    /// command's filters (currently `search()`).  Each impl writes a discriminator
    /// followed by length-prefixed bytes of its semantic fields, deterministically.
    ///
    /// The hash MUST NOT include ephemeral state such as the active `EphContext`
    /// (those vary across calls and would fragment the cache without changing
    /// the result).  Hash only fields that affect which rows are matched.
    fn hash_into(&self, h: &mut Sha256);
}

/// Composable filter tree (AND, OR, NOT, Leaf) for building Diesel WHERE clauses.
#[derive(Clone, Debug)]
pub enum CompositeFilter {
    And(Vec<CompositeFilter>),
    Or(Vec<CompositeFilter>),
    Not(Box<CompositeFilter>),
    Leaf(Box<dyn FilterLeaf>),
}

impl CompositeFilter {
    /// Shorthand: wrap a FilterLeaf in a Leaf variant.
    pub fn leaf(leaf: impl FilterLeaf + 'static) -> Self {
        CompositeFilter::Leaf(Box::new(leaf))
    }

    /// Shorthand: AND of children. Flattens single-child case.
    pub fn and(children: Vec<CompositeFilter>) -> Self {
        match children.len() {
            0 => CompositeFilter::And(vec![]),
            1 => children.into_iter().next().unwrap(),
            _ => CompositeFilter::And(children),
        }
    }

    /// Shorthand: OR of children. Flattens single-child case.
    pub fn or(children: Vec<CompositeFilter>) -> Self {
        match children.len() {
            0 => CompositeFilter::Or(vec![]),
            1 => children.into_iter().next().unwrap(),
            _ => CompositeFilter::Or(children),
        }
    }

    /// Shorthand: NOT. Eliminates double negation.
    pub fn not(inner: CompositeFilter) -> Self {
        match inner {
            CompositeFilter::Not(inner_inner) => *inner_inner,
            _ => CompositeFilter::Not(Box::new(inner)),
        }
    }
}

// Fold helpers — compose N boxed bool expressions with AND or OR.
fn fold_and<QS: 'static>(
    exprs: Vec<Box<dyn BoxableExpression<QS, Pg, SqlType = Bool>>>,
) -> Option<Box<dyn BoxableExpression<QS, Pg, SqlType = Bool>>> {
    let mut iter = exprs.into_iter();
    let first = iter.next()?;
    Some(iter.fold(first, |acc, e| {
        Box::new(acc.and(e)) as Box<dyn BoxableExpression<QS, Pg, SqlType = Bool>>
    }))
}

fn fold_or<QS: 'static>(
    exprs: Vec<Box<dyn BoxableExpression<QS, Pg, SqlType = Bool>>>,
) -> Option<Box<dyn BoxableExpression<QS, Pg, SqlType = Bool>>> {
    let mut iter = exprs.into_iter();
    let first = iter.next()?;
    Some(iter.fold(first, |acc, e| {
        Box::new(acc.or(e)) as Box<dyn BoxableExpression<QS, Pg, SqlType = Bool>>
    }))
}

// Composition methods — produce a single Diesel expression from the filter tree.
//
// Semantics of None:
//   None = "no constraint on this query context" = match everything.
//
// For AND: dropping None children is correct (identity element).
// For OR:  if ANY child is None (unconstrained), the whole OR is unconstrained.
// For NOT: not(None) = None — negating "no constraint" is still "no constraint"
//          (we can't negate something that doesn't apply to this context).
macro_rules! compose_method_vis {
    ($method:ident, $leaf_method:ident, $expr_type:ty) => {
        pub fn $method(&self, vis: &EphVisibility) -> Option<$expr_type> {
            match self {
                CompositeFilter::Leaf(leaf) => leaf.$leaf_method(vis),
                CompositeFilter::And(children) => {
                    // None children are dropped (no constraint = identity for AND).
                    // Empty result from fold_and = None = match everything.
                    let exprs: Vec<_> = children.iter().filter_map(|c| c.$method(vis)).collect();
                    fold_and(exprs)
                }
                CompositeFilter::Or(children) => {
                    if children.is_empty() {
                        // Empty OR = match nothing.
                        return Some(Box::new(OwnedSql::<Bool>::new("FALSE".into())) as $expr_type);
                    }
                    let mut exprs = Vec::with_capacity(children.len());
                    for child in children {
                        match child.$method(vis) {
                            // A child with no constraint means "match everything" —
                            // OR with "everything" is "everything".
                            None => return None,
                            Some(expr) => exprs.push(expr),
                        }
                    }
                    fold_or(exprs)
                }
                CompositeFilter::Not(inner) => inner
                    .$method(vis)
                    .map(|e| Box::new(diesel::dsl::not(e)) as $expr_type),
            }
        }
    };
}

impl CompositeFilter {
    compose_method_vis!(compose_current, current_expr, CurrentBoolExpr);
    compose_method_vis!(compose_parents, parents_expr, ParentsBoolExpr);
    compose_method_vis!(compose_children, children_expr, ChildrenBoolExpr);
    compose_method_vis!(compose_has_parents, has_parents_expr, HasParentsBoolExpr);
    compose_method_vis!(compose_has_children, has_children_expr, HasChildrenBoolExpr);

    /// Object-level composition for `search()`; tokenless on purpose — the
    /// `objects_expr` contract requires chain-independence.
    pub fn compose_objects(&self) -> Option<ObjectsBoolExpr> {
        match self {
            CompositeFilter::Leaf(leaf) => leaf.objects_expr(),
            CompositeFilter::And(children) => {
                let exprs: Vec<_> = children
                    .iter()
                    .filter_map(|c| c.compose_objects())
                    .collect();
                fold_and(exprs)
            }
            CompositeFilter::Or(children) => {
                if children.is_empty() {
                    return Some(Box::new(OwnedSql::<Bool>::new("FALSE".into())) as ObjectsBoolExpr);
                }
                let mut exprs = Vec::with_capacity(children.len());
                for child in children {
                    match child.compose_objects() {
                        None => return None,
                        Some(expr) => exprs.push(expr),
                    }
                }
                fold_or(exprs)
            }
            CompositeFilter::Not(inner) => inner
                .compose_objects()
                .map(|e| Box::new(diesel::dsl::not(e)) as ObjectsBoolExpr),
        }
    }

    /// Whether composing `compose_has_children` would return Some — the
    /// shape probe for the has_children CTE fast path, WITHOUT building
    /// (and discarding) the boxed expression tree.  Mirrors the compose
    /// macro's None-propagation rules exactly: AND drops None children,
    /// OR is Some iff non-empty and all children Some, NOT propagates.
    /// Leaves must keep their Some/None decision independent of the
    /// visibility token (all current impls decide from their own fields).
    pub fn constrains_has_children(&self) -> bool {
        match self {
            CompositeFilter::Leaf(leaf) => leaf.constrains_has_children(),
            CompositeFilter::And(children) => children
                .iter()
                .any(CompositeFilter::constrains_has_children),
            CompositeFilter::Or(children) => {
                !children.is_empty()
                    && children
                        .iter()
                        .all(CompositeFilter::constrains_has_children)
            }
            CompositeFilter::Not(inner) => inner.constrains_has_children(),
        }
    }

    /// True when any leaf's SQL is chain-dependent beyond selected-row
    /// visibility — the loader then uses one Combined query instead of the
    /// persistent/eph partition (see [`FilterLeaf::is_chain_dependent`]).
    pub fn is_chain_dependent(&self) -> bool {
        match self {
            CompositeFilter::Leaf(leaf) => leaf.is_chain_dependent(),
            CompositeFilter::And(children) | CompositeFilter::Or(children) => {
                children.iter().any(CompositeFilter::is_chain_dependent)
            }
            CompositeFilter::Not(inner) => inner.is_chain_dependent(),
        }
    }

    /// Canonical hash of the filter tree.  Verbs that build an ephemeral layer
    /// whose contents depend on the surrounding command's filters (currently
    /// `search()`) mix this into their layer cache key so that different
    /// filter compositions produce different layers.
    ///
    /// Recursion encodes the tree shape: a one-byte discriminator per variant,
    /// a length prefix for And/Or, and `hash_into` of each child.
    pub fn hash_into(&self, h: &mut Sha256) {
        match self {
            CompositeFilter::Leaf(leaf) => {
                h.update([0u8]);
                leaf.hash_into(h);
            }
            CompositeFilter::And(children) => {
                h.update([1u8]);
                h.update((children.len() as u32).to_le_bytes());
                for c in children {
                    c.hash_into(h);
                }
            }
            CompositeFilter::Or(children) => {
                h.update([2u8]);
                h.update((children.len() as u32).to_le_bytes());
                for c in children {
                    c.hash_into(h);
                }
            }
            CompositeFilter::Not(inner) => {
                h.update([3u8]);
                inner.hash_into(h);
            }
        }
    }
}

// ============================================================================
// FilterLeaf implementations — one per mixin struct
// ============================================================================

/// Extract the last normalized token from a symbol name, matching the DB trigger's
/// `subpath(symbol_path, nlevel(symbol_path) - 1)` computation.
/// Falls back to "unknown" to match `symbol_name_to_ltree`'s COALESCE behavior.
fn extract_leaf_token(name: &str, dot_is_separator: bool) -> String {
    use std::borrow::Cow;
    let normalized: Cow<str> = if dot_is_separator {
        Cow::Borrowed(name)
    } else {
        Cow::Owned(name.replace('.', "_"))
    };
    normalize_symbol_tokens(&normalized)
        .pop()
        .unwrap_or_else(|| "unknown".to_string())
}

#[derive(Debug, Clone)]
pub struct CompoundNameMixin {
    lquery: Option<String>,
    leaf_token: Option<String>,
}

impl CompoundNameMixin {
    pub fn new(compound_name: &str) -> Self {
        Self::with_options(compound_name, false, true)
    }

    pub fn new_leaf_anchored(compound_name: &str) -> Self {
        Self::with_options(compound_name, true, true)
    }

    pub fn with_options(compound_name: &str, leaf_anchored: bool, dot_is_separator: bool) -> Self {
        let leaf_token = if leaf_anchored {
            Some(extract_leaf_token(compound_name, dot_is_separator))
        } else {
            None
        };
        Self {
            lquery: build_lquery(compound_name, leaf_anchored, dot_is_separator),
            leaf_token,
        }
    }
}

impl FilterLeaf for CompoundNameMixin {
    fn current_expr(&self, _vis: &EphVisibility) -> Option<CurrentBoolExpr> {
        let mut parts: Vec<CurrentBoolExpr> = vec![];
        if let Some(ref leaf) = self.leaf_token {
            parts.push(Box::new(
                index_schema::symbols::dsl::leaf_name.eq(leaf.clone()),
            ));
        }
        if let Some(ref lquery) = self.lquery {
            parts.push(Box::new(OwnedSql::<Bool>::new(ltree_filter_sql(
                "symbols.symbol_path",
                lquery,
            ))));
        }
        fold_and(parts)
    }

    fn hash_into(&self, h: &mut Sha256) {
        h.update(b"CompoundName");
        match &self.lquery {
            Some(s) => {
                h.update([1u8]);
                h.update((s.len() as u32).to_le_bytes());
                h.update(s.as_bytes());
            }
            None => h.update([0u8]),
        }
        match &self.leaf_token {
            Some(s) => {
                h.update([1u8]);
                h.update((s.len() as u32).to_le_bytes());
                h.update(s.as_bytes());
            }
            None => h.update([0u8]),
        }
    }
}

/// LeafNameMixin - filters symbols by the last label of their symbol_path.
#[derive(Debug, Clone)]
pub struct LeafNameMixin {
    leaf_name: String,
}

impl LeafNameMixin {
    pub fn new(name: &str, dot_is_separator: bool) -> Self {
        Self {
            leaf_name: extract_leaf_token(name, dot_is_separator),
        }
    }
}

impl FilterLeaf for LeafNameMixin {
    fn current_expr(&self, _vis: &EphVisibility) -> Option<CurrentBoolExpr> {
        Some(Box::new(
            index_schema::symbols::dsl::leaf_name.eq(self.leaf_name.clone()),
        ))
    }

    fn hash_into(&self, h: &mut Sha256) {
        h.update(b"LeafName");
        h.update((self.leaf_name.len() as u32).to_le_bytes());
        h.update(self.leaf_name.as_bytes());
    }
}

/// Push `c` onto `out`, escaping the three LIKE metacharacters (`\`, `%`,
/// `_`) with a backslash so they match literally when bound to LIKE/ILIKE.
pub(crate) fn push_like_escaped(out: &mut String, c: char) {
    match c {
        '\\' | '%' | '_' => {
            out.push('\\');
            out.push(c);
        }
        _ => out.push(c),
    }
}

/// LIKE-escape a whole string (see [`push_like_escaped`]).
pub(crate) fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        push_like_escaped(&mut out, c);
    }
    out
}

/// One piece of a leaf-name glob pattern.  Separator-free by construction:
/// patterns containing compound separators route to [`FullNameGlobMixin`]
/// instead, so invalid states are unrepresentable here.
#[derive(Debug, Clone, PartialEq)]
pub enum GlobPiece {
    /// A literal run of characters.
    Lit(String),
    /// The '*' glob wildcard: any run of characters, including empty.
    Star,
}

/// GlobNameMixin - filters symbols by a glob pattern over leaf_name.
/// Literals are normalized with the same pipeline as indexed leaf names
/// (see `normalize_leaf_fragment`), so the query side cannot disagree with
/// the index side.
#[derive(Debug, Clone)]
pub struct GlobNameMixin {
    /// LIKE pattern built from pieces: literals normalized and escaped,
    /// Star -> '%'.  Constructor-enforced invariant: always safe to bind.
    sql_pattern: String,
    /// Smart case: true iff any literal contains an uppercase letter.
    case_sensitive: bool,
}

impl GlobNameMixin {
    pub fn new(pieces: &[GlobPiece], dot_is_separator: bool, anchored: bool) -> Self {
        let mut sql_pattern = String::new();
        let mut case_sensitive = false;
        // Collapse adjacent wildcards so semantically identical patterns
        // produce identical SQL (and identical filter hashes).
        let mut last_was_wildcard = false;
        if !anchored {
            sql_pattern.push('%');
            last_was_wildcard = true;
        }
        for piece in pieces {
            match piece {
                GlobPiece::Star => {
                    if !last_was_wildcard {
                        sql_pattern.push('%');
                        last_was_wildcard = true;
                    }
                }
                GlobPiece::Lit(text) => {
                    // Smart case is decided by the characters that actually
                    // participate in matching — normalization may strip an
                    // uppercase char (e.g. non-ASCII) from both sides.
                    let normalized = normalize_leaf_fragment(text, dot_is_separator);
                    case_sensitive |= smart_case_sensitive(&normalized);
                    if !normalized.is_empty() {
                        last_was_wildcard = false;
                    }
                    for c in normalized.chars() {
                        push_like_escaped(&mut sql_pattern, c);
                    }
                }
            }
        }
        if !anchored && !last_was_wildcard {
            sql_pattern.push('%');
        }
        Self {
            sql_pattern,
            case_sensitive,
        }
    }
}

impl FilterLeaf for GlobNameMixin {
    fn current_expr(&self, _vis: &EphVisibility) -> Option<CurrentBoolExpr> {
        let pattern = self.sql_pattern.clone();
        Some(if self.case_sensitive {
            Box::new(index_schema::symbols::dsl::leaf_name.like(pattern))
        } else {
            Box::new(index_schema::symbols::dsl::leaf_name.ilike(pattern))
        })
    }

    fn hash_into(&self, h: &mut Sha256) {
        h.update(b"GlobName");
        h.update((self.sql_pattern.len() as u32).to_le_bytes());
        h.update(self.sql_pattern.as_bytes());
        h.update([self.case_sensitive as u8]);
    }
}

/// FullNameGlobMixin - glob over the full symbols.name column.  Used for
/// patterns mixing wildcards with compound separators and for absolute-path
/// globs: literals match the raw stored name verbatim (no normalization),
/// '*' becomes '%'.  The pattern is anchored — the whole name must match;
/// "contains" is expressed with explicit wildcards (g"*foo*").
#[derive(Debug, Clone)]
pub struct FullNameGlobMixin {
    sql_pattern: String,
    case_sensitive: bool,
}

impl FullNameGlobMixin {
    pub fn new(pattern: &str) -> Self {
        let mut sql_pattern = String::with_capacity(pattern.len());
        // Collapse adjacent wildcards so semantically identical patterns
        // produce identical SQL (and identical filter hashes).
        let mut last_was_wildcard = false;
        for c in pattern.chars() {
            if c == '*' {
                if !last_was_wildcard {
                    sql_pattern.push('%');
                    last_was_wildcard = true;
                }
            } else {
                push_like_escaped(&mut sql_pattern, c);
                last_was_wildcard = false;
            }
        }
        Self {
            sql_pattern,
            case_sensitive: smart_case_sensitive(pattern),
        }
    }
}

impl FilterLeaf for FullNameGlobMixin {
    fn current_expr(&self, _vis: &EphVisibility) -> Option<CurrentBoolExpr> {
        let pattern = self.sql_pattern.clone();
        Some(if self.case_sensitive {
            Box::new(index_schema::symbols::dsl::name.like(pattern))
        } else {
            Box::new(index_schema::symbols::dsl::name.ilike(pattern))
        })
    }

    fn hash_into(&self, h: &mut Sha256) {
        h.update(b"FullNameGlob");
        h.update((self.sql_pattern.len() as u32).to_le_bytes());
        h.update(self.sql_pattern.as_bytes());
        h.update([self.case_sensitive as u8]);
    }
}

/// ExactNameMixin - filters symbols by exact name match.
#[derive(Debug, Clone)]
pub struct ExactNameMixin {
    name: String,
}

impl ExactNameMixin {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

impl FilterLeaf for ExactNameMixin {
    fn current_expr(&self, _vis: &EphVisibility) -> Option<CurrentBoolExpr> {
        Some(Box::new(
            index_schema::symbols::dsl::name.eq(self.name.clone()),
        ))
    }

    fn hash_into(&self, h: &mut Sha256) {
        h.update(b"ExactName");
        h.update((self.name.len() as u32).to_le_bytes());
        h.update(self.name.as_bytes());
    }
}

#[derive(Debug, Clone)]
pub struct SymbolInstanceIdMixin {
    instance_ids: Vec<i64>,
}

impl SymbolInstanceIdMixin {
    pub fn new(ids: &[SymbolInstanceId]) -> Self {
        Self {
            instance_ids: ids.iter().map(|id| Into::<i64>::into(*id)).collect(),
        }
    }
}

impl FilterLeaf for SymbolInstanceIdMixin {
    fn current_expr(&self, _vis: &EphVisibility) -> Option<CurrentBoolExpr> {
        Some(Box::new(
            index_schema::symbol_instances::dsl::id.eq_any(self.instance_ids.clone()),
        ))
    }

    fn hash_into(&self, h: &mut Sha256) {
        h.update(b"SymbolInstanceId");
        h.update((self.instance_ids.len() as u32).to_le_bytes());
        for id in &self.instance_ids {
            h.update(id.to_le_bytes());
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProjectFilterMixin {
    project_name: String,
}

impl ProjectFilterMixin {
    pub fn new(project_name: &str) -> Self {
        Self {
            project_name: project_name.to_string(),
        }
    }
}

impl FilterLeaf for ProjectFilterMixin {
    fn current_expr(&self, _vis: &EphVisibility) -> Option<CurrentBoolExpr> {
        Some(Box::new(
            index_schema::projects::dsl::project_name.eq(self.project_name.clone()),
        ))
    }

    /// Object-level constraint: restrict to objects belonging to a project whose
    /// `project_name` matches.  Encoded as `objects.project_id IN (SELECT id FROM
    /// projects WHERE project_name = $name)` so the resulting expression lives
    /// on the `objects` table alone and can be embedded in queries that don't
    /// otherwise join `projects` (notably `search()`'s content scan).
    fn objects_expr(&self) -> Option<ObjectsBoolExpr> {
        Some(Box::new(
            index_schema::objects::dsl::project_id.eq_any(
                index_schema::projects::dsl::projects
                    .select(index_schema::projects::dsl::id)
                    .filter(
                        index_schema::projects::dsl::project_name.eq(self.project_name.clone()),
                    ),
            ),
        ))
    }

    fn hash_into(&self, h: &mut Sha256) {
        h.update(b"Project");
        h.update((self.project_name.len() as u32).to_le_bytes());
        h.update(self.project_name.as_bytes());
    }
}

/// DirectOnlyMixin — filters children/has_children to "direct" only.
#[derive(Debug, Clone)]
pub struct DirectOnlyMixin;

impl DirectOnlyMixin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DirectOnlyMixin {
    fn default() -> Self {
        Self::new()
    }
}

impl FilterLeaf for DirectOnlyMixin {
    fn has_children_expr(&self, vis: &EphVisibility) -> Option<HasChildrenBoolExpr> {
        Some(Box::new(
            EphSqlFragment::<Bool>::builder()
                .sql(
                    "NOT EXISTS (\
                        SELECT 1 FROM index.symbol_instances mid \
                        JOIN index.symbols mid_sym ON mid.symbol = mid_sym.id \
                        JOIN index.symbol_types mid_type ON mid_sym.symbol_type = mid_type.id \
                        WHERE mid.object_id = symbol_instances.object_id \
                          AND symbol_instances.offset_range @> mid.offset_range \
                          AND mid.offset_range @> contained_instances.offset_range \
                          AND mid.offset_range != symbol_instances.offset_range \
                          AND mid.offset_range != contained_instances.offset_range \
                          AND mid.id != symbol_instances.id \
                          AND mid.id != contained_instances.id \
                          AND symbol_types.level >= mid_type.level \
                          AND ",
                )
                .eph_visibility("mid.layer", vis)
                .sql(" AND ")
                .eph_visibility("mid_sym.layer", vis)
                .sql(")")
                .build(),
        ))
    }

    fn children_expr(&self, vis: &EphVisibility) -> Option<ChildrenBoolExpr> {
        Some(Box::new(
            EphSqlFragment::<Bool>::builder()
                .sql("NOT EXISTS (\
                        SELECT 1 FROM index.symbol_instances container \
                        JOIN index.symbols cont_sym ON container.symbol = cont_sym.id \
                        JOIN index.symbol_types cont_type ON cont_sym.symbol_type = cont_type.id \
                        JOIN index.symbol_types parent_type ON parent_type.id = parent_symbols.symbol_type \
                        WHERE container.object_id = parent_decls.object_id \
                          AND parent_decls.offset_range @> container.offset_range \
                          AND container.offset_range @> symbol_refs.from_offset_range \
                          AND container.offset_range != parent_decls.offset_range \
                          AND container.id != parent_decls.id \
                          AND cont_type.level <= parent_type.level \
                          AND ")
                .eph_visibility("container.layer", vis)
                .sql(" AND ")
                .eph_visibility("cont_sym.layer", vis)
                .sql(")")
                .build()
        ))
    }

    fn hash_into(&self, h: &mut Sha256) {
        // EphContext is ephemeral state, excluded by contract.
        h.update(b"DirectOnly");
    }

    fn is_chain_dependent(&self) -> bool {
        // The NOT EXISTS subqueries consult chain-visible mid/container
        // rows: an eph instance can change a persistent row's membership,
        // so the persistent/eph partition union would not equal the legacy
        // result.  Forces the Combined fallback.
        true
    }

    fn constrains_has_children(&self) -> bool {
        true // has_children_expr always returns Some
    }
}

/// InnermostOnlyMixin — filters has_parents to innermost container only.
#[derive(Debug, Clone)]
pub struct InnermostOnlyMixin;

impl InnermostOnlyMixin {
    pub fn new() -> Self {
        Self
    }
}

impl Default for InnermostOnlyMixin {
    fn default() -> Self {
        Self::new()
    }
}

impl FilterLeaf for InnermostOnlyMixin {
    fn has_parents_expr(&self, vis: &EphVisibility) -> Option<HasParentsBoolExpr> {
        Some(Box::new(
            EphSqlFragment::<Bool>::builder()
                .sql(
                    "NOT EXISTS (\
                        SELECT 1 FROM index.symbol_instances mid \
                        JOIN index.symbols mid_sym ON mid_sym.id = mid.symbol \
                        WHERE mid.object_id = container_instances.object_id \
                          AND container_instances.offset_range @> mid.offset_range \
                          AND mid.offset_range @> symbol_instances.offset_range \
                          AND mid.offset_range != container_instances.offset_range \
                          AND mid.offset_range != symbol_instances.offset_range \
                          AND mid.id != container_instances.id \
                          AND mid.id != symbol_instances.id \
                          AND ",
                )
                .eph_visibility("mid.layer", vis)
                .sql(" AND ")
                .eph_visibility("mid_sym.layer", vis)
                .sql(")")
                .build(),
        ))
    }

    fn hash_into(&self, h: &mut Sha256) {
        // EphContext is ephemeral state, excluded by contract.
        h.update(b"InnermostOnly");
    }

    fn is_chain_dependent(&self) -> bool {
        true // see DirectOnlyMixin::is_chain_dependent
    }
}

/// OuterParentFilterMixin — filters out nested parent instances from REFS queries.
#[derive(Debug, Clone)]
pub struct OuterParentFilterMixin {
    parent_ids: Vec<i64>,
}

impl OuterParentFilterMixin {
    pub fn new(parent_ids: &[i64]) -> Self {
        Self {
            parent_ids: parent_ids.to_vec(),
        }
    }
}

impl FilterLeaf for OuterParentFilterMixin {
    fn children_expr(&self, vis: &EphVisibility) -> Option<ChildrenBoolExpr> {
        if self.parent_ids.is_empty() {
            return None;
        }
        Some(Box::new(
            EphSqlFragment::<Bool>::builder()
                .sql(
                    "NOT EXISTS (\
                        SELECT 1 FROM index.symbol_instances op \
                        WHERE op.id = ANY(",
                )
                .bind(self.parent_ids.clone())
                .sql(
                    ") \
                          AND op.id != parent_decls.id \
                          AND op.object_id = parent_decls.object_id \
                          AND op.offset_range @> parent_decls.offset_range \
                          AND op.offset_range != parent_decls.offset_range \
                          AND ",
                )
                .eph_visibility("op.layer", vis)
                .sql(")")
                .build(),
        ))
    }

    fn hash_into(&self, h: &mut Sha256) {
        // EphContext is ephemeral state, excluded by contract.
        h.update(b"OuterParent");
        h.update((self.parent_ids.len() as u32).to_le_bytes());
        for id in &self.parent_ids {
            h.update(id.to_le_bytes());
        }
    }

    fn is_chain_dependent(&self) -> bool {
        // Only when the NOT EXISTS subquery is actually emitted —
        // children_expr returns None for empty parent_ids, and a leaf
        // that renders no SQL cannot make the query chain-dependent.
        !self.parent_ids.is_empty()
    }
}

/// SymbolTypeMixin - filters symbols by type ID.
#[derive(Debug, Clone)]
pub struct SymbolTypeMixin {
    symbol_type_id: i32,
}

impl SymbolTypeMixin {
    pub fn new(symbol_type_id: i32) -> Self {
        Self { symbol_type_id }
    }
}

impl FilterLeaf for SymbolTypeMixin {
    fn current_expr(&self, _vis: &EphVisibility) -> Option<CurrentBoolExpr> {
        Some(Box::new(
            index_schema::symbols::dsl::symbol_type.eq(self.symbol_type_id),
        ))
    }

    fn hash_into(&self, h: &mut Sha256) {
        h.update(b"SymbolType");
        h.update(self.symbol_type_id.to_le_bytes());
    }
}

/// DefaultSymbolTypeMixin - filters symbols by multiple type IDs (OR condition).
#[derive(Debug, Clone)]
pub struct DefaultSymbolTypeMixin {
    symbol_type_ids: Vec<i32>,
}

impl DefaultSymbolTypeMixin {
    pub fn new(symbol_type_ids: Vec<i32>) -> Self {
        Self { symbol_type_ids }
    }
}

impl FilterLeaf for DefaultSymbolTypeMixin {
    fn current_expr(&self, _vis: &EphVisibility) -> Option<CurrentBoolExpr> {
        Some(Box::new(
            index_schema::symbols::dsl::symbol_type.eq_any(self.symbol_type_ids.clone()),
        ))
    }

    fn hash_into(&self, h: &mut Sha256) {
        h.update(b"DefaultSymbolType");
        h.update((self.symbol_type_ids.len() as u32).to_le_bytes());
        for id in &self.symbol_type_ids {
            h.update(id.to_le_bytes());
        }
    }
}

/// PackageDescendantLeaf — matches descendants of a package path (excluding exact match).
/// Used by IgnoreVerb for package exclusion via `Not(Leaf(PackageDescendantLeaf))`.
#[derive(Debug, Clone)]
pub struct PackageDescendantLeaf {
    base_path: String,
}

impl PackageDescendantLeaf {
    pub fn new(package: &str) -> Option<Self> {
        let path = symbol_name_to_path(package);
        if path == "unknown" {
            None
        } else {
            Some(Self { base_path: path })
        }
    }
}

impl FilterLeaf for PackageDescendantLeaf {
    fn current_expr(&self, _vis: &EphVisibility) -> Option<CurrentBoolExpr> {
        // "descendants only, not exact match" — sanitized via symbol_name_to_path
        Some(Box::new(OwnedSql::<Bool>::new(format!(
            "( '{}'::ltree @> symbols.symbol_path ) AND (symbols.symbol_path <> '{}')",
            self.base_path, self.base_path
        ))))
    }

    fn hash_into(&self, h: &mut Sha256) {
        h.update(b"PackageDescendant");
        h.update((self.base_path.len() as u32).to_le_bytes());
        h.update(self.base_path.as_bytes());
    }
}

/// Symbol type constants
pub const SYMBOL_TYPE_FUNCTION: i32 = 1;
pub const SYMBOL_TYPE_FILE: i32 = 2;
pub const SYMBOL_TYPE_MODULE: i32 = 3;
pub const SYMBOL_TYPE_DIRECTORY: i32 = 4;
pub const SYMBOL_TYPE_TYPE: i32 = 5;
pub const SYMBOL_TYPE_DATA: i32 = 6;
pub const SYMBOL_TYPE_MACRO: i32 = 7;
pub const SYMBOL_TYPE_FIELD: i32 = 8;
/// Content-anchored symbol — emitted by verbs that materialise a byte range in
/// source content rather than a real language-level symbol (loc, search).
pub const SYMBOL_TYPE_CONTENT: i32 = 9;

/// Instance type constants
pub const INSTANCE_TYPE_DEFINITION: i32 = 1;
pub const INSTANCE_TYPE_DECLARATION: i32 = 2;
pub const INSTANCE_TYPE_EXPANSION: i32 = 3;
pub const INSTANCE_TYPE_SENTINEL: i32 = 4;
pub const INSTANCE_TYPE_CONTAINMENT: i32 = 5;
pub const INSTANCE_TYPE_SOURCE: i32 = 6;
pub const INSTANCE_TYPE_HEADER: i32 = 7;
pub const INSTANCE_TYPE_BUILD: i32 = 8;
pub const INSTANCE_TYPE_FILE: i32 = 9;
pub const INSTANCE_TYPE_DOCUMENTATION: i32 = 10;

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(s: &str) -> GlobPiece {
        GlobPiece::Lit(s.to_string())
    }

    #[test]
    fn glob_translates_wildcards_to_percent() {
        let mixin = GlobNameMixin::new(
            &[
                GlobPiece::Star,
                lit("func"),
                GlobPiece::Star,
                lit("bar"),
                GlobPiece::Star,
            ],
            true,
            true,
        );
        assert_eq!(mixin.sql_pattern, "%func%bar%");
    }

    #[test]
    fn glob_normalizes_literals_like_indexed_leaf_names() {
        // '-' and other stripped chars are removed at index time; the glob
        // literal must go through the same pipeline or it can never match.
        let mixin = GlobNameMixin::new(&[lit("my-app"), GlobPiece::Star], false, true);
        assert_eq!(mixin.sql_pattern, "myapp%");

        let mixin = GlobNameMixin::new(&[lit("O'Brien"), GlobPiece::Star], true, true);
        assert_eq!(mixin.sql_pattern, "OBrien%");

        // '_' survives normalization and is LIKE-escaped.
        let mixin = GlobNameMixin::new(&[lit("do_run")], true, true);
        assert_eq!(mixin.sql_pattern, r"do\_run");
    }

    #[test]
    fn glob_dot_matches_normalized_underscore_for_files() {
        // askld folds Separator('.') into a literal "." for files/dirs.
        let mixin = GlobNameMixin::new(&[GlobPiece::Star, lit("."), lit("go")], false, true);
        assert_eq!(mixin.sql_pattern, r"%\_go");
    }

    #[test]
    fn glob_unanchored_wraps_and_collapses_wildcards() {
        // match="contains" (anchored = false) wraps in implicit wildcards,
        // and adjacent wildcards collapse so the pattern stays minimal.
        let mixin = GlobNameMixin::new(&[lit("run")], true, false);
        assert_eq!(mixin.sql_pattern, "%run%");

        // A leading '*' next to the implicit prefix '%' must not double up.
        let mixin = GlobNameMixin::new(&[GlobPiece::Star, lit("run")], true, false);
        assert_eq!(mixin.sql_pattern, "%run%");
    }

    #[test]
    fn glob_smart_case_uses_normalized_literal() {
        let sensitive = GlobNameMixin::new(&[lit("Foo"), GlobPiece::Star], true, true);
        assert!(sensitive.case_sensitive);

        let insensitive = GlobNameMixin::new(&[lit("foo"), GlobPiece::Star], true, true);
        assert!(!insensitive.case_sensitive);

        // An uppercase character stripped by normalization (non-ASCII) must
        // not force case-sensitivity on the surviving lowercase pattern.
        let mixin = GlobNameMixin::new(&[lit("Öfoo"), GlobPiece::Star], true, true);
        assert_eq!(mixin.sql_pattern, "foo%");
        assert!(!mixin.case_sensitive);
    }

    #[test]
    fn glob_hash_distinct_from_leaf_name() {
        let mut glob_hash = Sha256::new();
        GlobNameMixin::new(&[lit("foo")], true, true).hash_into(&mut glob_hash);

        let mut leaf_hash = Sha256::new();
        LeafNameMixin::new("foo", true).hash_into(&mut leaf_hash);

        assert_ne!(glob_hash.finalize(), leaf_hash.finalize());
    }

    #[test]
    fn full_name_glob_is_anchored() {
        // Anchored: no implicit surrounding wildcards. A trailing '*'
        // becomes a trailing '%'; a name lacking it must not match.
        let mixin = FullNameGlobMixin::new("fmt.Print*");
        assert_eq!(mixin.sql_pattern, "fmt.Print%");
        assert!(mixin.case_sensitive);

        // Literals match the raw stored name verbatim — '(' passes through,
        // '*' is a wildcard, so a pasted Go name still matches its source.
        let mixin = FullNameGlobMixin::new("(*Kubelet).Run");
        assert_eq!(mixin.sql_pattern, "(%Kubelet).Run");
    }

    #[test]
    fn full_name_glob_collapses_and_escapes() {
        // Adjacent wildcards collapse; LIKE metacharacters are escaped.
        let mixin = FullNameGlobMixin::new(r"*50%_\done*");
        assert_eq!(mixin.sql_pattern, r"%50\%\_\\done%");
    }

    #[test]
    fn full_name_glob_hash_distinct_from_leaf_glob() {
        let mut full_hash = Sha256::new();
        FullNameGlobMixin::new("foo").hash_into(&mut full_hash);

        let mut leaf_hash = Sha256::new();
        GlobNameMixin::new(&[lit("foo")], true, true).hash_into(&mut leaf_hash);

        assert_ne!(full_hash.finalize(), leaf_hash.finalize());
    }
}
