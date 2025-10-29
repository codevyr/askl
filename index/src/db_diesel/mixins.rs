use anyhow::Result;
use diesel::dsl::Eq;
use diesel::helper_types::{AsSelect, InnerJoinQuerySource};
use diesel::internal::table_macro::{BoxedSelectStatement, FromClause};
use diesel::prelude::*;
use diesel::query_source::{Alias, AliasedField};
use diesel::sql_types::{Integer, Text};
use diesel::sqlite::Sqlite;
use diesel::{debug_query, sql_query};

use crate::models_diesel::{Declaration, File, Module, Project, Symbol, SymbolRef};
use crate::symbols::{clean_and_split_string, DeclarationId};

use super::dsl::GlobMethods;
use super::Connection;

diesel::alias! {
    pub const CHILDREN_SYMBOLS_ALIAS: Alias<ChildrenSymbolsAlias> =
        crate::schema_diesel::symbols as children_symbols;
    pub const CHILDREN_DECLS_ALIAS: Alias<ChildrenDeclsAlias> =
        crate::schema_diesel::declarations as children_decls;
    pub const PARENT_SYMBOLS_ALIAS: Alias<ParentSymbolsAlias> =
        crate::schema_diesel::symbols as parent_symbols;
    pub const PARENT_DECLS_ALIAS: Alias<ParentDeclsAlias> =
        crate::schema_diesel::declarations as parent_decls;
}

type SymbolDeclarationJoinSource = InnerJoinQuerySource<
    crate::schema_diesel::symbols::table,
    crate::schema_diesel::declarations::table,
>;

type SymbolDeclarationModuleJoinSource =
    InnerJoinQuerySource<SymbolDeclarationJoinSource, crate::schema_diesel::modules::table>;

type SymbolDeclarationModuleProjectJoin = InnerJoinQuerySource<
    SymbolDeclarationModuleJoinSource,
    crate::schema_diesel::projects::table,
    Eq<
        crate::schema_diesel::projects::columns::id,
        crate::schema_diesel::modules::columns::project_id,
    >,
>;

type SymbolDeclarationModuleProjectFileJoin = InnerJoinQuerySource<
    SymbolDeclarationModuleProjectJoin,
    crate::schema_diesel::files::table,
    Eq<
        crate::schema_diesel::files::columns::id,
        crate::schema_diesel::declarations::columns::file_id,
    >,
>;

type SelectionTuple = (
    AsSelect<Symbol, Sqlite>,
    AsSelect<Declaration, Sqlite>,
    AsSelect<Module, Sqlite>,
    AsSelect<File, Sqlite>,
    AsSelect<Project, Sqlite>,
);

pub type CurrentQuery<'a> = BoxedSelectStatement<
    'a,
    SelectionTuple,
    FromClause<SymbolDeclarationModuleProjectFileJoin>,
    Sqlite,
>;

type DeclarationColumnsSqlType = (
    Integer,
    Integer,
    Integer,
    Integer,
    Integer,
    Integer,
    Integer,
    Integer,
);

type SymbolColumnsSqlType = (Integer, Text, Integer, Integer);

type ParentSelectionTuple = (
    AsSelect<SymbolRef, Sqlite>,
    AsSelect<Symbol, Sqlite>,
    AsSelect<Declaration, Sqlite>,
    DeclarationColumnsSqlType, // We cannot use AsSelect<Declaration, Sqlite> here due to ambiguity
);

type SymbolRefSymbolJoin = InnerJoinQuerySource<
    crate::schema_diesel::symbol_refs::table,
    crate::schema_diesel::symbols::table,
    Eq<
        crate::schema_diesel::symbol_refs::columns::to_symbol,
        crate::schema_diesel::symbols::columns::id,
    >,
>;

type SymbolRefSymbolDeclarationJoin = InnerJoinQuerySource<
    SymbolRefSymbolJoin,
    crate::schema_diesel::declarations::table,
    Eq<
        crate::schema_diesel::symbols::columns::id,
        crate::schema_diesel::declarations::columns::symbol,
    >,
>;

type ParentDeclOn = Eq<
    AliasedField<ParentDeclsAlias, crate::schema_diesel::declarations::columns::id>,
    crate::schema_diesel::symbol_refs::columns::from_decl,
>;

type ChildSymbolOn = Eq<
    AliasedField<ChildrenSymbolsAlias, crate::schema_diesel::symbols::columns::id>,
    crate::schema_diesel::symbol_refs::columns::to_symbol,
>;

type SymbolRefChildrenJoin = InnerJoinQuerySource<
    crate::schema_diesel::symbol_refs::table,
    Alias<ChildrenSymbolsAlias>,
    ChildSymbolOn,
>;

type ChildActualSymbolOn = Eq<
    crate::schema_diesel::symbol_refs::columns::to_symbol,
    crate::schema_diesel::symbols::columns::id,
>;

type SymbolRefChildrenActualJoin = InnerJoinQuerySource<
    SymbolRefChildrenJoin,
    crate::schema_diesel::symbols::table,
    ChildActualSymbolOn,
>;

type SymbolRefChildrenActualDeclarationJoin = InnerJoinQuerySource<
    SymbolRefChildrenActualJoin,
    crate::schema_diesel::declarations::table,
    Eq<
        crate::schema_diesel::symbols::columns::id,
        crate::schema_diesel::declarations::columns::symbol,
    >,
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
    Sqlite,
>;

type ChildSelectionTuple = (
    SymbolColumnsSqlType,
    AsSelect<Symbol, Sqlite>,
    AsSelect<Declaration, Sqlite>,
    AsSelect<SymbolRef, Sqlite>,
    AsSelect<File, Sqlite>,
);

type ParentSymbolOn = Eq<
    AliasedField<ParentSymbolsAlias, crate::schema_diesel::symbols::columns::id>,
    AliasedField<ParentDeclsAlias, crate::schema_diesel::declarations::columns::symbol>,
>;

type ParentFileOn = Eq<
    crate::schema_diesel::files::columns::id,
    AliasedField<ParentDeclsAlias, crate::schema_diesel::declarations::columns::file_id>,
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
    crate::schema_diesel::files::table,
    ParentFileOn,
>;

pub type ChildrenQuery<'a> = BoxedSelectStatement<
    'a,
    ChildSelectionTuple,
    FromClause<SymbolRefSymbolDeclParentDeclParentSymbolFileJoin>,
    Sqlite,
>;

#[derive(Debug, Clone, PartialEq, QueryableByName)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct SymbolRowid {
    #[diesel(sql_type = Integer)]
    pub rowid: i32,
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
    pub compound_name: Vec<String>,
    pub name_pattern: String,

    matched_symbols: Vec<SymbolRowid>,
}

impl CompoundNameMixin {
    pub fn new(compound_name: &str) -> Self {
        let name_slice = clean_and_split_string(&compound_name);
        Self {
            compound_name: name_slice,
            name_pattern: String::new(),
            matched_symbols: Vec::new(),
        }
    }
}

impl SymbolSearchMixin for CompoundNameMixin {
    fn enter(&mut self, connection: &mut Connection) -> Result<()> {
        let fts_name_pattern = self.compound_name.join(" AND ");
        let name_pattern = self.compound_name.join("*");
        self.name_pattern = format!("*{}*", name_pattern);

        let matched_symbols_query =
            sql_query("SELECT rowid FROM symbols_fts WHERE symbols_fts MATCH ?")
                .bind::<Text, _>(&fts_name_pattern);

        println!(
            "Executing FTS query: {:?}",
            debug_query::<Sqlite, _>(&matched_symbols_query)
        );

        self.matched_symbols = {
            let _matched_symbols_query: tracing::span::EnteredSpan =
                tracing::info_span!("matched_symbols").entered();
            matched_symbols_query
                .load::<SymbolRowid>(connection)
                .map_err(|e| anyhow::anyhow!("Failed to query FTS table: {}", e))?
        };
        println!("Matched {} symbols", self.matched_symbols.len());
        println!("Searching for symbols with name pattern: {fts_name_pattern}");
        Ok(())
    }

    fn filter_current<'a>(
        &self,
        _connection: &mut Connection,
        query: CurrentQuery<'a>,
    ) -> Result<CurrentQuery<'a>> {
        use crate::schema_diesel::*;

        let symbol_ids: Vec<i32> = self.matched_symbols.iter().map(|s| s.rowid).collect();

        Ok(query
            .filter(symbols::dsl::id.eq_any(symbol_ids))
            .filter(symbols::dsl::name.glob(self.name_pattern.clone())))
    }

    fn filter_parents<'a>(
        &self,
        _connection: &mut Connection,
        query: ParentsQuery<'a>,
    ) -> Result<ParentsQuery<'a>> {
        use crate::schema_diesel::symbols;

        let symbol_ids: Vec<i32> = self.matched_symbols.iter().map(|s| s.rowid).collect();

        Ok(query
            .filter(
                CHILDREN_SYMBOLS_ALIAS
                    .field(symbols::dsl::id)
                    .eq_any(symbol_ids),
            )
            .filter(
                CHILDREN_SYMBOLS_ALIAS
                    .field(symbols::dsl::name)
                    .glob(self.name_pattern.clone()),
            ))
    }

    fn filter_children<'a>(
        &self,
        _connection: &mut Connection,
        query: ChildrenQuery<'a>,
    ) -> Result<ChildrenQuery<'a>> {
        let symbol_ids: Vec<i32> = self.matched_symbols.iter().map(|s| s.rowid).collect();

        Ok(query
            .filter(
                PARENT_SYMBOLS_ALIAS
                    .field(crate::schema_diesel::symbols::dsl::id)
                    .eq_any(symbol_ids),
            )
            .filter(
                PARENT_SYMBOLS_ALIAS
                    .field(crate::schema_diesel::symbols::dsl::name)
                    .glob(self.name_pattern.clone()),
            ))
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
