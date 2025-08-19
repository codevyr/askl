use diesel::prelude::*;

#[derive(Clone, Queryable, Selectable, Identifiable, Associations, Debug, PartialEq, Eq, Hash)]
#[diesel(table_name = crate::schema_diesel::declarations)]
#[diesel(belongs_to(Symbol, foreign_key = symbol))]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Declaration {
    pub id: i32,
    pub symbol: i32,
    pub file_id: i32,
    pub symbol_type: i32,
    pub line_start: i32,
    pub col_start: i32,
    pub line_end: i32,
    pub col_end: i32,
}

#[derive(Clone, Queryable, Selectable, Identifiable, Associations, Debug, PartialEq, Eq, Hash)]
#[diesel(table_name = crate::schema_diesel::files)]
#[diesel(belongs_to(Module, foreign_key = module))]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct File {
    pub id: i32,
    pub module: i32,
    pub module_path: String,
    pub filesystem_path: String,
    pub filetype: String,
}

#[derive(Clone, Queryable, Selectable, Identifiable, Debug, PartialEq, Eq, Hash)]
#[diesel(table_name = crate::schema_diesel::modules)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Module {
    pub id: i32,
    pub module_name: String,
}

#[derive(
    Clone,
    Queryable,
    Selectable,
    Associations,
    Identifiable,
    Debug,
    PartialEq,
    QueryableByName,
    Eq,
    Hash,
)]
#[diesel(table_name = crate::schema_diesel::symbols)]
#[diesel(belongs_to(Module, foreign_key = module))]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Symbol {
    pub id: i32,
    pub name: String,
    pub module: i32,
    pub symbol_scope: i32,
}

#[derive(Clone, Queryable, Selectable, Debug, PartialEq)]
#[diesel(table_name = crate::schema_diesel::symbol_refs)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct SymbolRef {
    pub rowid: i32,
    pub from_decl: i32,
    pub to_symbol: i32,
    pub from_line: i32,
    pub from_col_start: i32,
    pub from_col_end: i32,
}
