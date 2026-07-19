use std::marker::PhantomData;

use diesel::dsl::Eq;
use diesel::expression::{BoxableExpression, ValidGrouping, is_aggregate};
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
use crate::symbols::{symbol_name_to_path, build_lquery, normalize_symbol_tokens, SymbolInstanceId};

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

pub type CurrentQuery<'a> = BoxedSelectStatement<
    'a,
    SelectionTuple,
    FromClause<SymbolInstanceProjectObjectJoin>,
    Pg,
>;

type SymbolInstanceColumnsSqlType = (BigInt, BigInt, Integer, Int4range, Integer, Nullable<BigInt>);

type SymbolColumnsSqlType = (BigInt, Text, Ltree, Integer, Integer, Nullable<Integer>, Text, Nullable<BigInt>);  // (id, name, symbol_path, project_id, symbol_type, symbol_scope, leaf_name, eph_layer)

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
    AsSelect<Symbol, Pg>,           // child_symbol (current)
    AsSelect<SymbolInstance, Pg>,   // child_instance (current)
    SymbolColumnsSqlType,           // parent_symbol (container)
    SymbolInstanceColumnsSqlType,   // parent_instance (container)
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

pub type HasParentsQuery<'a> = BoxedSelectStatement<
    'a,
    HasParentsSelectionTuple,
    FromClause<HasParentsJoinSource>,
    Pg,
>;

// has_children: find symbols contained by current symbols
// Query structure: symbol_instances -> symbols -> symbol_types -> objects -> contained_instances -> contained_symbols -> contained_types
type HasChildrenSelectionTuple = (
    AsSelect<Symbol, Pg>,           // parent_symbol (current)
    AsSelect<SymbolInstance, Pg>,   // parent_instance (current)
    SymbolColumnsSqlType,           // child_symbol (contained)
    SymbolInstanceColumnsSqlType,   // child_instance (contained)
    AsSelect<Object, Pg>,           // parent_object
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

pub type HasChildrenQuery<'a> = BoxedSelectStatement<
    'a,
    HasChildrenSelectionTuple,
    FromClause<HasChildrenJoinSource>,
    Pg,
>;

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
        Self { sql, _marker: PhantomData }
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

impl<ST: 'static + Send + diesel::sql_types::SingleValue, QS> SelectableExpression<QS> for OwnedSql<ST> {}
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
///     .eph_visibility("op.eph_layer", &self.eph)
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
        EphSqlBuilder { parts: Vec::new(), _marker: PhantomData }
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

    /// Emit `(<column> IS NULL OR <column> = ANY($N))` where `$N` is the
    /// position of the bound `eph` array.  Each call binds the array
    /// separately; if the same array is referenced from multiple call sites
    /// in one fragment, the wire payload is duplicated — acceptable for the
    /// short arrays we deal with in practice.
    pub(crate) fn eph_visibility(mut self, column: &str, eph: &EphContext) -> Self {
        let sql = format!("({} IS NULL OR {} = ANY(", column, column);
        match self.parts.last_mut() {
            Some(EphSqlPart::Sql(buf)) => buf.push_str(&sql),
            _ => self.parts.push(EphSqlPart::Sql(sql)),
        }
        self.parts.push(EphSqlPart::BindI64Array(eph.as_slice().to_vec()));
        self.parts.push(EphSqlPart::Sql("))".to_string()));
        self
    }

    pub(crate) fn build(self) -> EphSqlFragment<ST> {
        EphSqlFragment { parts: self.parts, _marker: PhantomData }
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
pub trait FilterLeaf: std::fmt::Debug + FilterLeafClone + Send + Sync {
    fn current_expr(&self) -> Option<CurrentBoolExpr> { None }
    fn parents_expr(&self) -> Option<ParentsBoolExpr> { None }
    fn children_expr(&self) -> Option<ChildrenBoolExpr> { None }
    fn has_parents_expr(&self) -> Option<HasParentsBoolExpr> { None }
    fn has_children_expr(&self) -> Option<HasChildrenBoolExpr> { None }

    /// Object-level predicate, used by `search()` to scope its content scan.
    /// Returns `None` for filters that only constrain at the symbol/instance layer.
    fn objects_expr(&self) -> Option<ObjectsBoolExpr> { None }

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
    exprs: Vec<Box<dyn BoxableExpression<QS, Pg, SqlType = Bool>>>
) -> Option<Box<dyn BoxableExpression<QS, Pg, SqlType = Bool>>> {
    let mut iter = exprs.into_iter();
    let first = iter.next()?;
    Some(iter.fold(first, |acc, e| {
        Box::new(acc.and(e)) as Box<dyn BoxableExpression<QS, Pg, SqlType = Bool>>
    }))
}

fn fold_or<QS: 'static>(
    exprs: Vec<Box<dyn BoxableExpression<QS, Pg, SqlType = Bool>>>
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
macro_rules! compose_method {
    ($method:ident, $leaf_method:ident, $expr_type:ty) => {
        pub fn $method(&self) -> Option<$expr_type> {
            match self {
                CompositeFilter::Leaf(leaf) => leaf.$leaf_method(),
                CompositeFilter::And(children) => {
                    // None children are dropped (no constraint = identity for AND).
                    // Empty result from fold_and = None = match everything.
                    let exprs: Vec<_> = children.iter().filter_map(|c| c.$method()).collect();
                    fold_and(exprs)
                }
                CompositeFilter::Or(children) => {
                    if children.is_empty() {
                        // Empty OR = match nothing.
                        return Some(Box::new(OwnedSql::<Bool>::new("FALSE".into())) as $expr_type);
                    }
                    let mut exprs = Vec::with_capacity(children.len());
                    for child in children {
                        match child.$method() {
                            // A child with no constraint means "match everything" —
                            // OR with "everything" is "everything".
                            None => return None,
                            Some(expr) => exprs.push(expr),
                        }
                    }
                    fold_or(exprs)
                }
                CompositeFilter::Not(inner) => {
                    inner.$method().map(|e| Box::new(diesel::dsl::not(e)) as $expr_type)
                }
            }
        }
    }
}

impl CompositeFilter {
    compose_method!(compose_current, current_expr, CurrentBoolExpr);
    compose_method!(compose_parents, parents_expr, ParentsBoolExpr);
    compose_method!(compose_children, children_expr, ChildrenBoolExpr);
    compose_method!(compose_has_parents, has_parents_expr, HasParentsBoolExpr);
    compose_method!(compose_has_children, has_children_expr, HasChildrenBoolExpr);
    compose_method!(compose_objects, objects_expr, ObjectsBoolExpr);

    /// Canonical hash of the filter tree.  Verbs that build an ephemeral layer
    /// whose contents depend on the surrounding command's filters (currently
    /// `search()`) mix this into their eph_layer cache key so that different
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
                for c in children { c.hash_into(h); }
            }
            CompositeFilter::Or(children) => {
                h.update([2u8]);
                h.update((children.len() as u32).to_le_bytes());
                for c in children { c.hash_into(h); }
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
    normalize_symbol_tokens(&normalized).pop()
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
    fn current_expr(&self) -> Option<CurrentBoolExpr> {
        let mut parts: Vec<CurrentBoolExpr> = vec![];
        if let Some(ref leaf) = self.leaf_token {
            parts.push(Box::new(index_schema::symbols::dsl::leaf_name.eq(leaf.clone())));
        }
        if let Some(ref lquery) = self.lquery {
            parts.push(Box::new(OwnedSql::<Bool>::new(
                ltree_filter_sql("symbols.symbol_path", lquery)
            )));
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
        Self { leaf_name: extract_leaf_token(name, dot_is_separator) }
    }
}

impl FilterLeaf for LeafNameMixin {
    fn current_expr(&self) -> Option<CurrentBoolExpr> {
        Some(Box::new(index_schema::symbols::dsl::leaf_name.eq(self.leaf_name.clone())))
    }

    fn hash_into(&self, h: &mut Sha256) {
        h.update(b"LeafName");
        h.update((self.leaf_name.len() as u32).to_le_bytes());
        h.update(self.leaf_name.as_bytes());
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
    fn current_expr(&self) -> Option<CurrentBoolExpr> {
        Some(Box::new(index_schema::symbols::dsl::name.eq(self.name.clone())))
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
    fn current_expr(&self) -> Option<CurrentBoolExpr> {
        Some(Box::new(
            index_schema::symbol_instances::dsl::id.eq_any(self.instance_ids.clone())
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
    fn current_expr(&self) -> Option<CurrentBoolExpr> {
        Some(Box::new(
            index_schema::projects::dsl::project_name.eq(self.project_name.clone())
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
                    .filter(index_schema::projects::dsl::project_name.eq(self.project_name.clone()))
            )
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
pub struct DirectOnlyMixin {
    eph: EphContext,
}

impl DirectOnlyMixin {
    pub fn new(eph: &EphContext) -> Self {
        Self { eph: eph.clone() }
    }
}

impl FilterLeaf for DirectOnlyMixin {
    fn has_children_expr(&self) -> Option<HasChildrenBoolExpr> {
        Some(Box::new(
            EphSqlFragment::<Bool>::builder()
                .sql("NOT EXISTS (\
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
                          AND ")
                .eph_visibility("mid.eph_layer", &self.eph)
                .sql(" AND ")
                .eph_visibility("mid_sym.eph_layer", &self.eph)
                .sql(")")
                .build()
        ))
    }

    fn children_expr(&self) -> Option<ChildrenBoolExpr> {
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
                .eph_visibility("container.eph_layer", &self.eph)
                .sql(" AND ")
                .eph_visibility("cont_sym.eph_layer", &self.eph)
                .sql(")")
                .build()
        ))
    }

    fn hash_into(&self, h: &mut Sha256) {
        // EphContext is ephemeral state, excluded by contract.
        h.update(b"DirectOnly");
    }
}

/// InnermostOnlyMixin — filters has_parents to innermost container only.
#[derive(Debug, Clone)]
pub struct InnermostOnlyMixin {
    eph: EphContext,
}

impl InnermostOnlyMixin {
    pub fn new(eph: &EphContext) -> Self {
        Self { eph: eph.clone() }
    }
}

impl FilterLeaf for InnermostOnlyMixin {
    fn has_parents_expr(&self) -> Option<HasParentsBoolExpr> {
        Some(Box::new(
            EphSqlFragment::<Bool>::builder()
                .sql("NOT EXISTS (\
                        SELECT 1 FROM index.symbol_instances mid \
                        JOIN index.symbols mid_sym ON mid_sym.id = mid.symbol \
                        WHERE mid.object_id = container_instances.object_id \
                          AND container_instances.offset_range @> mid.offset_range \
                          AND mid.offset_range @> symbol_instances.offset_range \
                          AND mid.offset_range != container_instances.offset_range \
                          AND mid.offset_range != symbol_instances.offset_range \
                          AND mid.id != container_instances.id \
                          AND mid.id != symbol_instances.id \
                          AND ")
                .eph_visibility("mid.eph_layer", &self.eph)
                .sql(" AND ")
                .eph_visibility("mid_sym.eph_layer", &self.eph)
                .sql(")")
                .build()
        ))
    }

    fn hash_into(&self, h: &mut Sha256) {
        // EphContext is ephemeral state, excluded by contract.
        h.update(b"InnermostOnly");
    }
}

/// OuterParentFilterMixin — filters out nested parent instances from REFS queries.
#[derive(Debug, Clone)]
pub struct OuterParentFilterMixin {
    parent_ids: Vec<i64>,
    eph: EphContext,
}

impl OuterParentFilterMixin {
    pub fn new(parent_ids: &[i64], eph: &EphContext) -> Self {
        Self {
            parent_ids: parent_ids.to_vec(),
            eph: eph.clone(),
        }
    }
}

impl FilterLeaf for OuterParentFilterMixin {
    fn children_expr(&self) -> Option<ChildrenBoolExpr> {
        if self.parent_ids.is_empty() {
            return None;
        }
        Some(Box::new(
            EphSqlFragment::<Bool>::builder()
                .sql("NOT EXISTS (\
                        SELECT 1 FROM index.symbol_instances op \
                        WHERE op.id = ANY(")
                .bind(self.parent_ids.clone())
                .sql(") \
                          AND op.id != parent_decls.id \
                          AND op.object_id = parent_decls.object_id \
                          AND op.offset_range @> parent_decls.offset_range \
                          AND op.offset_range != parent_decls.offset_range \
                          AND ")
                .eph_visibility("op.eph_layer", &self.eph)
                .sql(")")
                .build()
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
    fn current_expr(&self) -> Option<CurrentBoolExpr> {
        Some(Box::new(index_schema::symbols::dsl::symbol_type.eq(self.symbol_type_id)))
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
    fn current_expr(&self) -> Option<CurrentBoolExpr> {
        Some(Box::new(index_schema::symbols::dsl::symbol_type.eq_any(self.symbol_type_ids.clone())))
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
    fn current_expr(&self) -> Option<CurrentBoolExpr> {
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

