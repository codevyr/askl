use diesel::prelude::*;
use std::collections::Bound;

#[derive(Clone, Queryable, Selectable, Identifiable, Associations, Debug, PartialEq, Eq, Hash)]
#[diesel(table_name = crate::schema_diesel::declarations)]
#[diesel(belongs_to(Symbol, foreign_key = symbol))]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Declaration {
    pub id: i32,
    pub symbol: i32,
    pub file_id: i32,
    pub symbol_type: i32,
    pub offset_range: (Bound<i32>, Bound<i32>),
}

#[derive(Clone, Queryable, Selectable, Identifiable, Associations, Debug, PartialEq, Eq, Hash)]
#[diesel(table_name = crate::schema_diesel::files)]
#[diesel(belongs_to(Module, foreign_key = module))]
#[diesel(belongs_to(Project, foreign_key = project_id))]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct File {
    pub id: i32,
    pub project_id: i32,
    pub module: Option<i32>,
    pub directory_id: i32,
    pub module_path: String,
    pub filesystem_path: String,
    pub filetype: String,
    pub content_hash: String,
}

#[derive(Clone, Queryable, Selectable, Identifiable, Debug, PartialEq, Eq, Hash)]
#[diesel(table_name = crate::schema_diesel::modules)]
#[diesel(belongs_to(Project, foreign_key = project_id))]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Module {
    pub id: i32,
    pub module_name: String,
    pub project_id: i32,
}

#[derive(Clone, Queryable, Selectable, Identifiable, Debug, PartialEq, Eq, Hash)]
#[diesel(table_name = crate::schema_diesel::projects)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Project {
    pub id: i32,
    pub project_name: String,
    pub root_path: String,
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
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Symbol {
    pub id: i32,
    pub name: String,
    pub symbol_path: String,
    pub module: i32,
    pub symbol_scope: i32,
}

#[derive(Clone, Queryable, Selectable, Debug, PartialEq)]
#[diesel(table_name = crate::schema_diesel::symbol_refs)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct SymbolRef {
    pub id: i32,
    pub to_symbol: i32,
    pub from_file: i32,
    pub from_offset_range: (Bound<i32>, Bound<i32>),
}
