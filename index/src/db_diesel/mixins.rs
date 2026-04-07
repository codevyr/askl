use anyhow::Result;
use diesel::dsl::{sql, Eq};
use diesel::expression::SqlLiteral;
use diesel::helper_types::{AsSelect, InnerJoinQuerySource};
use diesel::internal::table_macro::{BoxedSelectStatement, FromClause};
use diesel::pg::Pg;
use diesel::prelude::*;
use diesel::query_source::{Alias, AliasedField};
use diesel::sql_types::{Bool, Int4range, Integer, Text};

use crate::ltree::Ltree;
use crate::models_diesel::{Object, Project, Symbol, SymbolInstance, SymbolRef};
use crate::schema_diesel as index_schema;
use crate::symbols::{symbol_name_to_path, symbol_query_to_lquery, build_lquery, SymbolInstanceId};

use super::Connection;

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

type SymbolInstanceColumnsSqlType = (Integer, Integer, Integer, Int4range, Integer);

type SymbolColumnsSqlType = (Integer, Text, Ltree, Integer, Integer, diesel::sql_types::Nullable<Integer>);  // (id, name, symbol_path, project_id, symbol_type, symbol_scope)

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

/// Trait for composable query filters used by `find_symbol()`.
///
/// `filter_current` constrains which symbols the initial query matches.
/// After the current query runs, `find_symbol()` extracts the surviving instance IDs
/// (`current_instance_ids`) and applies them as inline filters to all follow-up queries.
///
/// Because of this, follow-up methods (`filter_parents`, `filter_children`, etc.) should
/// only add filters that constrain the *relationship* side — e.g. the caller's type in
/// `filter_parents`. Do NOT re-filter the current symbol in follow-up methods;
/// `current_instance_ids` already handles that.
pub trait SymbolSearchMixin: std::fmt::Debug {
    fn enter(&self, _connection: &mut Connection) -> Result<()> {
        Ok(())
    }

    fn filter_current<'a>(
        &self,
        _connection: &mut Connection,
        query: CurrentQuery<'a>,
    ) -> Result<CurrentQuery<'a>> {
        Ok(query)
    }

    fn filter_parents<'a>(
        &self,
        _connection: &mut Connection,
        query: ParentsQuery<'a>,
    ) -> Result<ParentsQuery<'a>> {
        Ok(query)
    }

    fn filter_children<'a>(
        &self,
        _connection: &mut Connection,
        query: ChildrenQuery<'a>,
    ) -> Result<ChildrenQuery<'a>> {
        Ok(query)
    }

    /// Filter has_parents query (find containers of current symbols)
    /// The "current" symbol is the child in the containment relationship.
    fn filter_has_parents<'a>(
        &self,
        _connection: &mut Connection,
        query: HasParentsQuery<'a>,
    ) -> Result<HasParentsQuery<'a>> {
        Ok(query)
    }

    /// Filter has_children query (find symbols contained by current symbols)
    /// The "current" symbol is the parent in the containment relationship.
    fn filter_has_children<'a>(
        &self,
        _connection: &mut Connection,
        query: HasChildrenQuery<'a>,
    ) -> Result<HasChildrenQuery<'a>> {
        Ok(query)
    }
}

#[derive(Debug, Clone)]
pub struct IgnoreFilterMixin {
    name_lquery: Option<String>,
    package_path: Option<String>,
}

impl IgnoreFilterMixin {
    pub fn new(name: Option<&str>, package: Option<&str>) -> Self {
        let name_lquery = name.and_then(symbol_query_to_lquery);
        let mut package_path = None;
        if let Some(value) = package {
            let path = symbol_name_to_path(value);
            if path != "unknown" {
                package_path = Some(path);
            }
        }
        Self {
            name_lquery,
            package_path,
        }
    }

    fn apply_filter<Q>(mut query: Q, column: &str, lquery: &Option<String>) -> Q
    where
        Q: diesel::query_dsl::methods::FilterDsl<SqlLiteral<Bool>, Output = Q>,
    {
        if let Some(lquery) = lquery {
            let filter_sql = format!("NOT ({})", ltree_filter_sql(column, lquery));
            query = diesel::query_dsl::methods::FilterDsl::filter(query, sql::<Bool>(&filter_sql));
        }
        query
    }

    fn apply_package_filter<Q>(mut query: Q, column: &str, base_path: &Option<String>) -> Q
    where
        Q: diesel::query_dsl::methods::FilterDsl<SqlLiteral<Bool>, Output = Q>,
    {
        if let Some(base_path) = base_path {
            // Exclude descendants of the package path, but keep the exact match.
            let filter_sql = format!(
                "NOT (( '{}'::ltree @> {} ) AND ({} <> '{}'))",
                base_path, column, column, base_path
            );
            query = diesel::query_dsl::methods::FilterDsl::filter(query, sql::<Bool>(&filter_sql));
        }
        query
    }
}

impl SymbolSearchMixin for IgnoreFilterMixin {
    fn filter_current<'a>(
        &self,
        _connection: &mut Connection,
        query: CurrentQuery<'a>,
    ) -> Result<CurrentQuery<'a>> {
        let query = Self::apply_filter(query, "symbols.symbol_path", &self.name_lquery);
        let query = Self::apply_package_filter(query, "symbols.symbol_path", &self.package_path);
        Ok(query)
    }

    // No filter_parents/filter_children: these would only re-filter
    // the current symbol, which is already constrained by current_instance_ids.
}

#[derive(Debug, Clone)]
pub struct CompoundNameMixin {
    pub raw_name: String,
    lquery: Option<String>,
}

impl CompoundNameMixin {
    pub fn new(compound_name: &str) -> Self {
        Self::with_options(compound_name, false, true)
    }

    pub fn new_leaf_anchored(compound_name: &str) -> Self {
        Self::with_options(compound_name, true, true)
    }

    pub fn with_options(compound_name: &str, leaf_anchored: bool, dot_is_separator: bool) -> Self {
        Self {
            raw_name: compound_name.to_string(),
            lquery: build_lquery(compound_name, leaf_anchored, dot_is_separator),
        }
    }
}

impl SymbolSearchMixin for CompoundNameMixin {
    fn filter_current<'a>(
        &self,
        _connection: &mut Connection,
        query: CurrentQuery<'a>,
    ) -> Result<CurrentQuery<'a>> {
        if let Some(lquery) = &self.lquery {
            let filter_sql = ltree_filter_sql("symbols.symbol_path", lquery);
            Ok(query.filter(sql::<Bool>(&filter_sql)))
        } else {
            Ok(query)
        }
    }

    // No filter_parents/filter_children/filter_has_*: these would only re-filter
    // the current symbol, which is already constrained by current_instance_ids.
}

/// ExactNameMixin - filters symbols by exact name match.
/// Used for directory and file selectors where paths should match exactly.
#[derive(Debug, Clone)]
pub struct ExactNameMixin {
    pub name: String,
}

impl ExactNameMixin {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

impl SymbolSearchMixin for ExactNameMixin {
    fn filter_current<'a>(
        &self,
        _connection: &mut Connection,
        query: CurrentQuery<'a>,
    ) -> Result<CurrentQuery<'a>> {
        use index_schema::symbols;
        let name = self.name.clone();
        Ok(query.filter(symbols::dsl::name.eq(name)))
    }

    // No filter_parents/filter_children/filter_has_*: these would only re-filter
    // the current symbol, which is already constrained by current_instance_ids.
}

#[derive(Debug, Clone)]
pub struct SymbolInstanceIdMixin {
    pub instance_ids: Vec<i32>,
}

impl SymbolInstanceIdMixin {
    pub fn new(ids: &[SymbolInstanceId]) -> Self {
        Self {
            instance_ids: ids.iter().map(|id| Into::<i32>::into(*id)).collect(),
        }
    }
}

impl SymbolSearchMixin for SymbolInstanceIdMixin {
    fn filter_current<'a>(
        &self,
        _connection: &mut Connection,
        query: CurrentQuery<'a>,
    ) -> Result<CurrentQuery<'a>> {
        use crate::schema_diesel::symbol_instances;

        Ok(query.filter(symbol_instances::dsl::id.eq_any(self.instance_ids.clone())))
    }

    // No filter_parents/filter_children/filter_has_*: current_instance_ids
    // is applied inline in find_symbol() after the current query resolves.
}

// ModuleFilterMixin removed - modules are now symbols with type=MODULE
// Use symbol name filtering to find module symbols instead

#[derive(Debug, Clone)]
pub struct ProjectFilterMixin {
    pub project_name: String,
}

impl ProjectFilterMixin {
    pub fn new(project_name: &str) -> Self {
        Self {
            project_name: project_name.to_string(),
        }
    }
}

impl SymbolSearchMixin for ProjectFilterMixin {
    fn filter_current<'a>(
        &self,
        _connection: &mut Connection,
        query: CurrentQuery<'a>,
    ) -> Result<CurrentQuery<'a>> {
        use crate::schema_diesel::projects;

        Ok(query.filter(projects::dsl::project_name.eq(self.project_name.clone())))
    }
}

/// DirectOnlyMixin — filters children/has_children to "direct" only.
///
/// For HAS (containment): excludes children that have an intermediate container
/// between the parent and child at a valid nesting level.
///
/// For REFS: excludes references whose source location is inside a nested
/// container (e.g., a nested function) within the parent.
#[derive(Debug, Clone)]
pub struct DirectOnlyMixin;

impl SymbolSearchMixin for DirectOnlyMixin {
    fn filter_has_children<'a>(
        &self,
        _conn: &mut Connection,
        query: HasChildrenQuery<'a>,
    ) -> Result<HasChildrenQuery<'a>> {
        // Exclude contained children that have an intermediate container between
        // the parent (symbol_instances) and child (contained_instances).
        // symbol_types.level is available from the outer query's JOIN on symbol_types.
        Ok(query.filter(sql::<Bool>(
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
                  AND symbol_types.level >= mid_type.level\
            )",
        )))
    }

    fn filter_children<'a>(
        &self,
        _conn: &mut Connection,
        query: ChildrenQuery<'a>,
    ) -> Result<ChildrenQuery<'a>> {
        // Exclude refs whose source location is inside a nested container
        // within the parent (parent_decls). parent_symbols is available from
        // the outer query's JOIN.
        Ok(query.filter(sql::<Bool>(
            "NOT EXISTS (\
                SELECT 1 FROM index.symbol_instances container \
                JOIN index.symbols cont_sym ON container.symbol = cont_sym.id \
                JOIN index.symbol_types cont_type ON cont_sym.symbol_type = cont_type.id \
                JOIN index.symbol_types parent_type ON parent_type.id = parent_symbols.symbol_type \
                WHERE container.object_id = parent_decls.object_id \
                  AND parent_decls.offset_range @> container.offset_range \
                  AND container.offset_range @> symbol_refs.from_offset_range \
                  AND container.offset_range != parent_decls.offset_range \
                  AND container.id != parent_decls.id \
                  AND cont_type.level <= parent_type.level\
            )",
        )))
    }
}

/// InnermostOnlyMixin — filters has_parents to innermost container only.
///
/// Analogous to DirectOnlyMixin but for has_parents: excludes containers
/// that have an intermediate container between them and the child.
#[derive(Debug, Clone)]
pub struct InnermostOnlyMixin;

impl SymbolSearchMixin for InnermostOnlyMixin {
    fn filter_has_parents<'a>(
        &self,
        _conn: &mut Connection,
        query: HasParentsQuery<'a>,
    ) -> Result<HasParentsQuery<'a>> {
        Ok(query.filter(sql::<Bool>(
            "NOT EXISTS (\
                SELECT 1 FROM index.symbol_instances mid \
                WHERE mid.object_id = container_instances.object_id \
                  AND container_instances.offset_range @> mid.offset_range \
                  AND mid.offset_range @> symbol_instances.offset_range \
                  AND mid.offset_range != container_instances.offset_range \
                  AND mid.offset_range != symbol_instances.offset_range \
                  AND mid.id != container_instances.id \
                  AND mid.id != symbol_instances.id\
            )",
        )))
    }
}

/// OuterParentFilterMixin — filters out nested parent instances from REFS queries.
///
/// When `find_child_instance_ids` receives parent IDs that include nested instances
/// (e.g., `[foo, anon_in_foo]`), DirectOnlyMixin correctly excludes refs inside
/// `anon_in_foo` from `foo`'s perspective, but `anon_in_foo` independently contributes
/// its own direct refs — causing double-counting. This mixin excludes any parent
/// instance that is contained within another parent instance in the input set.
#[derive(Debug, Clone)]
pub struct OuterParentFilterMixin {
    parent_ids: Vec<i32>,
}

impl OuterParentFilterMixin {
    pub fn new(parent_ids: &[i32]) -> Self {
        Self { parent_ids: parent_ids.to_vec() }
    }
}

impl SymbolSearchMixin for OuterParentFilterMixin {
    fn filter_children<'a>(
        &self,
        _conn: &mut Connection,
        query: ChildrenQuery<'a>,
    ) -> Result<ChildrenQuery<'a>> {
        if self.parent_ids.is_empty() {
            return Ok(query);
        }
        let ids_csv = self.parent_ids.iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        Ok(query.filter(sql::<Bool>(&format!(
            "NOT EXISTS (\
                SELECT 1 FROM index.symbol_instances op \
                WHERE op.id IN ({ids_csv}) \
                  AND op.id != parent_decls.id \
                  AND op.object_id = parent_decls.object_id \
                  AND op.offset_range @> parent_decls.offset_range \
                  AND op.offset_range != parent_decls.offset_range\
            )"
        ))))
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

