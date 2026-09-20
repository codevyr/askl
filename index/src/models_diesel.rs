use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel::QueryableByName;
use std::collections::Bound;

#[derive(Clone, Queryable, Selectable, Identifiable, Debug, PartialEq, Eq, Hash)]
#[diesel(table_name = crate::schema_diesel::symbol_types)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct SymbolType {
    pub id: i32,
    pub name: String,
    pub level: i32,
    pub dot_is_separator: bool,
}

#[derive(Clone, Queryable, Selectable, Identifiable, Associations, Debug, PartialEq, Eq, Hash)]
#[diesel(table_name = crate::schema_diesel::symbol_instances)]
#[diesel(belongs_to(Symbol, foreign_key = symbol))]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct SymbolInstance {
    pub id: i64,
    pub symbol: i64,
    pub object_id: i32,
    /// The EXTENT: the authoritative containment span.
    pub offset_range: (Bound<i32>, Bound<i32>),
    pub instance_type: i32,
    pub layer: i64,
    /// Depth in this object's containment tree, counting symbol instances
    /// only.  A direct child is at exactly parent + 1.  This is what orders
    /// compiler-synthesised symbols, which ranges cannot: everything a macro
    /// generates shares one extent.
    pub nesting_level: i32,
    /// Depth-first entry/exit numbers within this object.  Descendants are
    /// exactly the instances whose `dfs_enter` lies strictly between these
    /// two, which makes `unnest` one btree range scan instead of a recursive
    /// walk -- and orders compiler-synthesised siblings that share an extent.
    pub dfs_enter: i64,
    pub dfs_exit: i64,
    /// Finer spans within `offset_range`; `None` where the indexer does not
    /// know one (synthesised symbols have no name token, and not every kind
    /// can be declared).
    pub name_range: Option<(Bound<i32>, Bound<i32>)>,
    pub signature_range: Option<(Bound<i32>, Bound<i32>)>,
    pub content_range: Option<(Bound<i32>, Bound<i32>)>,
}

#[derive(Clone, Queryable, Selectable, Identifiable, Associations, Debug, PartialEq, Eq, Hash)]
#[diesel(table_name = crate::schema_diesel::objects)]
#[diesel(belongs_to(Project, foreign_key = project_id))]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Object {
    pub id: i32,
    pub project_id: i32,
    // directory_id removed - directories are now symbols
    pub module_path: String,
    pub filesystem_path: String,
    pub filetype: String,
    pub content_hash: String,
    pub layer: i64,
}

#[derive(Clone, Queryable, Selectable, Identifiable, Debug, PartialEq, Eq, Hash)]
#[diesel(table_name = crate::schema_diesel::projects)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Project {
    pub id: i32,
    pub project_name: String,
    pub root_path: String,
    pub upload_status: String,
    pub root_layer_id: i64,
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
#[diesel(belongs_to(Project, foreign_key = project_id))]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Symbol {
    pub id: i64,
    pub name: String,
    pub symbol_path: String,
    pub project_id: i32,
    pub symbol_type: i32,
    pub symbol_scope: Option<i32>,
    pub leaf_name: String,
    pub layer: i64,
}

#[derive(Debug, QueryableByName)]
pub struct ContentRow {
    #[diesel(sql_type = diesel::sql_types::Binary)]
    pub content: Vec<u8>,
}

#[derive(Clone, Queryable, Selectable, Debug, PartialEq)]
#[diesel(table_name = crate::schema_diesel::symbol_refs)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct SymbolRef {
    pub id: i64,
    pub to_symbol: i64,
    pub from_object: i32,
    pub from_offset_range: (Bound<i32>, Bound<i32>),
    pub layer: i64,
    /// Depth of this reference; an instance owns the refs inside its extent
    /// at its own depth + 1.
    pub nesting_level: i32,
    /// Position in the same DFS sequence as this object's instances.  A ref is
    /// a point, not an interval.
    pub dfs_enter: i64,
    /// FULL span of the referencing construct (`printk(foo)` where
    /// `from_offset_range` covers only `printk`).  Display only.
    pub from_full_range: Option<(Bound<i32>, Bound<i32>)>,
}

#[derive(Clone, Queryable, Selectable, Identifiable, Debug, PartialEq)]
#[diesel(table_name = crate::schema_diesel::layers)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Layer {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub hash: Vec<u8>,
    pub kind: String,
    pub last_used: DateTime<Utc>,
}

#[derive(Insertable, Debug)]
#[diesel(table_name = crate::schema_diesel::layers)]
pub struct NewLayer<'a> {
    pub parent_id: Option<i64>,
    pub hash: &'a [u8],
    pub kind: &'a str,
}
