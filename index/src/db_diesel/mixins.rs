use anyhow::Result;
use diesel::dsl::{sql, Eq};
use diesel::helper_types::{AsSelect, InnerJoinQuerySource};
use diesel::internal::table_macro::{BoxedSelectStatement, FromClause};
use diesel::pg::Pg;
use diesel::prelude::*;
use diesel::query_source::{Alias, AliasedField};
use diesel::sql_types::{Bool, Int4range, Integer, Text};

use crate::ltree::Ltree;
use crate::models_diesel::{Declaration, File, Module, Project, Symbol, SymbolRef};
use crate::schema_diesel as index_schema;
use crate::symbols::{symbol_query_to_lquery, DeclarationId};

use super::Connection;

diesel::alias! {
    pub const CHILDREN_SYMBOLS_ALIAS: Alias<ChildrenSymbolsAlias> =
        index_schema::symbols as children_symbols;
    pub const CHILDREN_DECLS_ALIAS: Alias<ChildrenDeclsAlias> =
        index_schema::declarations as children_decls;
    pub const PARENT_SYMBOLS_ALIAS: Alias<ParentSymbolsAlias> =
        index_schema::symbols as parent_symbols;
    pub const PARENT_DECLS_ALIAS: Alias<ParentDeclsAlias> =
        index_schema::declarations as parent_decls;
}

type SymbolDeclarationJoinSource = InnerJoinQuerySource<
    index_schema::symbols::table,
    index_schema::declarations::table,
    Eq<index_schema::symbols::columns::id, index_schema::declarations::columns::symbol>,
>;

type SymbolDeclarationModuleJoinSource = InnerJoinQuerySource<
    SymbolDeclarationJoinSource,
    index_schema::modules::table,
    Eq<index_schema::symbols::columns::module, index_schema::modules::columns::id>,
>;

type SymbolDeclarationModuleProjectJoin = InnerJoinQuerySource<
    SymbolDeclarationModuleJoinSource,
    index_schema::projects::table,
    Eq<index_schema::projects::columns::id, index_schema::modules::columns::project_id>,
>;

type SymbolDeclarationModuleProjectFileJoin = InnerJoinQuerySource<
    SymbolDeclarationModuleProjectJoin,
    index_schema::files::table,
    Eq<index_schema::files::columns::id, index_schema::declarations::columns::file_id>,
>;

type SelectionTuple = (
    AsSelect<Symbol, Pg>,
    AsSelect<Declaration, Pg>,
    AsSelect<Module, Pg>,
    AsSelect<File, Pg>,
    AsSelect<Project, Pg>,
);

pub type CurrentQuery<'a> = BoxedSelectStatement<
    'a,
    SelectionTuple,
    FromClause<SymbolDeclarationModuleProjectFileJoin>,
    Pg,
>;

type DeclarationColumnsSqlType = (Integer, Integer, Integer, Integer, Int4range);

type SymbolColumnsSqlType = (Integer, Text, Ltree, Integer, Integer);

type ParentSelectionTuple = (
    AsSelect<SymbolRef, Pg>,
    AsSelect<Symbol, Pg>,
    AsSelect<Declaration, Pg>,
    DeclarationColumnsSqlType, // We cannot use AsSelect<Declaration, Pg> here due to ambiguity
);

type SymbolRefSymbolJoin = InnerJoinQuerySource<
    index_schema::symbol_refs::table,
    index_schema::symbols::table,
    Eq<index_schema::symbol_refs::columns::to_symbol, index_schema::symbols::columns::id>,
>;

type SymbolRefSymbolDeclarationJoin = InnerJoinQuerySource<
    SymbolRefSymbolJoin,
    index_schema::declarations::table,
    Eq<index_schema::symbols::columns::id, index_schema::declarations::columns::symbol>,
>;

type ParentDeclOn = Eq<
    AliasedField<ParentDeclsAlias, index_schema::declarations::columns::file_id>,
    index_schema::symbol_refs::columns::from_file,
>;

type ChildSymbolOn = Eq<
    AliasedField<ChildrenSymbolsAlias, index_schema::symbols::columns::id>,
    index_schema::symbol_refs::columns::to_symbol,
>;

type SymbolRefChildrenJoin = InnerJoinQuerySource<
    index_schema::symbol_refs::table,
    Alias<ChildrenSymbolsAlias>,
    ChildSymbolOn,
>;

type ChildActualSymbolOn =
    Eq<index_schema::symbol_refs::columns::to_symbol, index_schema::symbols::columns::id>;

type SymbolRefChildrenActualJoin =
    InnerJoinQuerySource<SymbolRefChildrenJoin, index_schema::symbols::table, ChildActualSymbolOn>;

type SymbolRefChildrenActualDeclarationJoin = InnerJoinQuerySource<
    SymbolRefChildrenActualJoin,
    index_schema::declarations::table,
    Eq<index_schema::symbols::columns::id, index_schema::declarations::columns::symbol>,
>;

type SymbolRefChildrenActualDeclarationParentDeclJoin = InnerJoinQuerySource<
    SymbolRefChildrenActualDeclarationJoin,
    Alias<ParentDeclsAlias>,
    ParentDeclOn,
>;

pub type ParentsQuery<'a> = BoxedSelectStatement<
    'a,
    ParentSelectionTuple,
    FromClause<SymbolRefChildrenActualDeclarationParentDeclJoin>,
    Pg,
>;

type ChildSelectionTuple = (
    SymbolColumnsSqlType,
    AsSelect<Symbol, Pg>,
    AsSelect<Declaration, Pg>,
    DeclarationColumnsSqlType,
    AsSelect<SymbolRef, Pg>,
    AsSelect<File, Pg>,
);

type ParentSymbolOn = Eq<
    AliasedField<ParentSymbolsAlias, index_schema::symbols::columns::id>,
    AliasedField<ParentDeclsAlias, index_schema::declarations::columns::symbol>,
>;

type ParentFileOn = Eq<
    index_schema::files::columns::id,
    AliasedField<ParentDeclsAlias, index_schema::declarations::columns::file_id>,
>;

type SymbolRefSymbolDeclParentDeclJoin =
    InnerJoinQuerySource<SymbolRefSymbolDeclarationJoin, Alias<ParentDeclsAlias>, ParentDeclOn>;

type SymbolRefSymbolDeclParentDeclParentSymbolJoin = InnerJoinQuerySource<
    SymbolRefSymbolDeclParentDeclJoin,
    Alias<ParentSymbolsAlias>,
    ParentSymbolOn,
>;

type SymbolRefSymbolDeclParentDeclParentSymbolFileJoin = InnerJoinQuerySource<
    SymbolRefSymbolDeclParentDeclParentSymbolJoin,
    index_schema::files::table,
    ParentFileOn,
>;

pub type ChildrenQuery<'a> = BoxedSelectStatement<
    'a,
    ChildSelectionTuple,
    FromClause<SymbolRefSymbolDeclParentDeclParentSymbolFileJoin>,
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
            let filter_sql = ltree_filter_sql("children_symbols.symbol_path", lquery);
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
        use crate::schema_diesel::declarations;

        Ok(query.filter(declarations::dsl::id.eq_any(self.decl_ids.clone())))
    }

    fn filter_parents<'a>(
        &self,
        _connection: &mut Connection,
        query: ParentsQuery<'a>,
    ) -> Result<ParentsQuery<'a>> {
        use crate::schema_diesel::declarations;

        Ok(query.filter(declarations::dsl::id.eq_any(self.decl_ids.clone())))
    }

    fn filter_children<'a>(
        &self,
        _connection: &mut Connection,
        query: ChildrenQuery<'a>,
    ) -> Result<ChildrenQuery<'a>> {
        use crate::schema_diesel::declarations;

        Ok(query.filter(
            PARENT_DECLS_ALIAS
                .field(declarations::dsl::id)
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
