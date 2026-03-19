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
use crate::models_diesel::{Module, Object, Project, Symbol, SymbolInstance, SymbolRef};
use crate::schema_diesel as index_schema;
use crate::symbols::{symbol_name_to_path, symbol_query_to_lquery, DeclarationId};

use super::Connection;

diesel::alias! {
    pub const PARENT_SYMBOLS_ALIAS: Alias<ParentSymbolsAlias> =
        index_schema::symbols as parent_symbols;
    pub const PARENT_DECLS_ALIAS: Alias<ParentDeclsAlias> =
        index_schema::symbol_instances as parent_decls;
}

type SymbolInstanceJoinSource = InnerJoinQuerySource<
    index_schema::symbols::table,
    index_schema::symbol_instances::table,
    Eq<index_schema::symbols::columns::id, index_schema::symbol_instances::columns::symbol>,
>;

type SymbolInstanceModuleJoinSource = InnerJoinQuerySource<
    SymbolInstanceJoinSource,
    index_schema::modules::table,
    Eq<index_schema::symbols::columns::module, index_schema::modules::columns::id>,
>;

type SymbolInstanceModuleProjectJoin = InnerJoinQuerySource<
    SymbolInstanceModuleJoinSource,
    index_schema::projects::table,
    Eq<index_schema::projects::columns::id, index_schema::modules::columns::project_id>,
>;

type SymbolInstanceModuleProjectObjectJoin = InnerJoinQuerySource<
    SymbolInstanceModuleProjectJoin,
    index_schema::objects::table,
    Eq<index_schema::objects::columns::id, index_schema::symbol_instances::columns::object_id>,
>;

type SelectionTuple = (
    AsSelect<Symbol, Pg>,
    AsSelect<SymbolInstance, Pg>,
    AsSelect<Module, Pg>,
    AsSelect<Object, Pg>,
    AsSelect<Project, Pg>,
);

pub type CurrentQuery<'a> = BoxedSelectStatement<
    'a,
    SelectionTuple,
    FromClause<SymbolInstanceModuleProjectObjectJoin>,
    Pg,
>;

type SymbolInstanceColumnsSqlType = (Integer, Integer, Integer, Integer, Int4range);

type SymbolColumnsSqlType = (Integer, Text, Ltree, Integer, Integer);

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
    FromClause<SymbolRefSymbolInstanceParentInstanceJoin>,
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

fn ltree_filter_sql(column: &str, lquery: &str) -> String {
    format!("{} ~ '{}'::lquery", column, lquery)
}

pub trait SymbolSearchMixin: std::fmt::Debug {
    fn enter(&mut self, _connection: &mut Connection) -> Result<()> {
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

    fn filter_parents<'a>(
        &self,
        _connection: &mut Connection,
        query: ParentsQuery<'a>,
    ) -> Result<ParentsQuery<'a>> {
        let query = Self::apply_filter(query, "symbols.symbol_path", &self.name_lquery);
        let query = Self::apply_package_filter(query, "symbols.symbol_path", &self.package_path);
        Ok(query)
    }

    fn filter_children<'a>(
        &self,
        _connection: &mut Connection,
        query: ChildrenQuery<'a>,
    ) -> Result<ChildrenQuery<'a>> {
        let query = Self::apply_filter(query, "parent_symbols.symbol_path", &self.name_lquery);
        let query =
            Self::apply_package_filter(query, "parent_symbols.symbol_path", &self.package_path);
        Ok(query)
    }
}

#[derive(Debug, Clone)]
pub struct CompoundNameMixin {
    pub raw_name: String,
    lquery: Option<String>,
}

impl CompoundNameMixin {
    pub fn new(compound_name: &str) -> Self {
        Self {
            raw_name: compound_name.to_string(),
            lquery: symbol_query_to_lquery(compound_name),
        }
    }
}

impl SymbolSearchMixin for CompoundNameMixin {
    fn enter(&mut self, _connection: &mut Connection) -> Result<()> {
        println!("Searching for symbols by lquery: {:?}", self.lquery);
        Ok(())
    }

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

    fn filter_parents<'a>(
        &self,
        _connection: &mut Connection,
        query: ParentsQuery<'a>,
    ) -> Result<ParentsQuery<'a>> {
        if let Some(lquery) = &self.lquery {
            let filter_sql = ltree_filter_sql("symbols.symbol_path", lquery);
            Ok(query.filter(sql::<Bool>(&filter_sql)))
        } else {
            Ok(query)
        }
    }

    fn filter_children<'a>(
        &self,
        _connection: &mut Connection,
        query: ChildrenQuery<'a>,
    ) -> Result<ChildrenQuery<'a>> {
        if let Some(lquery) = &self.lquery {
            let filter_sql = ltree_filter_sql("parent_symbols.symbol_path", lquery);
            Ok(query.filter(sql::<Bool>(&filter_sql)))
        } else {
            Ok(query)
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeclarationIdMixin {
    pub decl_ids: Vec<i32>,
}

impl DeclarationIdMixin {
    pub fn new(ids: &[DeclarationId]) -> Self {
        Self {
            decl_ids: ids.iter().map(|id| Into::<i32>::into(*id)).collect(),
        }
    }
}

impl SymbolSearchMixin for DeclarationIdMixin {
    fn enter(&mut self, _connection: &mut Connection) -> Result<()> {
        println!("Searching for symbols by decl_id: {:?}", self.decl_ids);
        Ok(())
    }

    fn filter_current<'a>(
        &self,
        _connection: &mut Connection,
        query: CurrentQuery<'a>,
    ) -> Result<CurrentQuery<'a>> {
        use crate::schema_diesel::symbol_instances;

        Ok(query.filter(symbol_instances::dsl::id.eq_any(self.decl_ids.clone())))
    }

    fn filter_parents<'a>(
        &self,
        _connection: &mut Connection,
        query: ParentsQuery<'a>,
    ) -> Result<ParentsQuery<'a>> {
        use crate::schema_diesel::symbol_instances;

        Ok(query.filter(symbol_instances::dsl::id.eq_any(self.decl_ids.clone())))
    }

    fn filter_children<'a>(
        &self,
        _connection: &mut Connection,
        query: ChildrenQuery<'a>,
    ) -> Result<ChildrenQuery<'a>> {
        use crate::schema_diesel::symbol_instances;

        Ok(query.filter(
            PARENT_DECLS_ALIAS
                .field(symbol_instances::dsl::id)
                .eq_any(self.decl_ids.clone()),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct ModuleFilterMixin {
    pub module_name: String,
}

impl ModuleFilterMixin {
    pub fn new(module_name: &str) -> Self {
        Self {
            module_name: module_name.to_string(),
        }
    }
}

impl SymbolSearchMixin for ModuleFilterMixin {
    fn filter_current<'a>(
        &self,
        _connection: &mut Connection,
        query: CurrentQuery<'a>,
    ) -> Result<CurrentQuery<'a>> {
        use crate::schema_diesel::modules;

        Ok(query.filter(modules::dsl::module_name.eq(self.module_name.clone())))
    }
}

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
