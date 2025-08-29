use crate::models_diesel::{Declaration, File, Module, Symbol, SymbolRef};
use crate::symbols::{
    DeclarationId, FileId, ModuleId, Occurrence, SymbolId, SymbolScope, SymbolType,
};
use anyhow::Result;
use diesel::connection::SimpleConnection;
use diesel::r2d2::{ConnectionManager, Pool, PooledConnection};
use diesel::sql_types::{Integer, Text};
use diesel::sqlite::Sqlite;
use diesel::{debug_query, SqliteConnection};
use diesel::{prelude::*, sql_query};

mod dsl {
    use diesel::{
        expression::{AsExpression, Expression},
        sql_types::{SingleValue, Text},
    };

    mod predicates {
        use diesel::sqlite::Sqlite;
        diesel::infix_operator!(Glob, " GLOB ", backend: Sqlite);
    }

    use self::predicates::Glob;

    pub trait GlobMethods
    where
        Self: Expression<SqlType = Text> + Sized,
    {
        fn glob<T>(self, other: T) -> Glob<Self, T::Expression>
        where
            Self::SqlType: diesel::sql_types::SqlType,
            T: AsExpression<Self::SqlType>,
        {
            Glob::new(self, other.as_expression())
        }
    }

    impl<T> GlobMethods for T
    where
        T: Expression<SqlType = diesel::sql_types::Text>,
        T::SqlType: SingleValue,
    {
    }
}

use self::dsl::GlobMethods;

pub struct Index {
    pool: Pool<ConnectionManager<SqliteConnection>>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ModuleFullDiesel {
    pub id: ModuleId,
    pub module_name: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FileFullDiesel {
    pub id: FileId,
    pub module: ModuleFullDiesel,
    pub module_path: String,
    pub filesystem_path: String,
    pub filetype: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ReferenceFullDiesel {
    pub from_decl: DeclarationId,
    pub to_symbol: SymbolId,
    pub occurrence: Occurrence,
}

#[derive(Debug, PartialEq, Eq)]
pub struct DeclarationFullDiesel {
    pub id: DeclarationId,
    pub symbol: SymbolId,
    pub name: String,
    pub symbol_scope: SymbolScope,
    pub file: FileFullDiesel,
    pub symbol_type: SymbolType,
    pub occurrence: Occurrence,

    pub children: Vec<ReferenceFullDiesel>,
    pub parents: Vec<ReferenceFullDiesel>,
}

#[derive(Debug, Clone, PartialEq, Hash, Eq)]
pub struct SelectionNode {
    pub symbol: Symbol,
    pub declaration: Declaration,
    pub module: Module,
    pub file: File,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceResult {
    pub symbol: Symbol,
    pub declaration: Declaration,
    pub symbol_ref: SymbolRef,
    pub from_file: File,
}

pub type ChildReference = ReferenceResult;

#[derive(Debug, Clone, PartialEq)]
pub struct ParentReference {
    pub from_file: File,
    pub from_symbol: Symbol,
    pub from_declaration: Declaration,
    pub to_symbol: Symbol,
    pub to_declaration: Declaration,
    pub symbol_ref: SymbolRef,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    pub nodes: Vec<SelectionNode>,
    pub parents: Vec<ParentReference>,
    pub children: Vec<ChildReference>,
}

impl Selection {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            parents: Vec::new(),
            children: Vec::new(),
        }
    }

    pub fn extend(&mut self, other: Selection) {
        self.nodes.extend(other.nodes);
        self.parents.extend(other.parents);
        self.children.extend(other.children);
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn get_decl_ids(&self) -> Vec<i32> {
        self.nodes.iter().map(|node| node.declaration.id).collect()
    }
}

#[derive(Debug, Clone, PartialEq, QueryableByName)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct SymbolRowid {
    #[diesel(sql_type = Integer)]
    pub rowid: i32,
}

impl Index {
    fn setup(connection: &mut PooledConnection<ConnectionManager<SqliteConnection>>) -> Result<()> {
        connection.batch_execute(
            r#"
        DROP TABLE IF EXISTS symbols_fts;
        CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
            name,                                    -- the tokenized text
            content='symbols',                        -- link to base table
            content_rowid='id',                       -- rowid = symbols.id
            tokenize='ascii'  -- default tokenization; '.' stays a separator
        );

        INSERT INTO symbols_fts(rowid, name)
        SELECT id, name FROM symbols;
        "#,
        )?;

        Ok(())
    }

    pub async fn connect(database: &str) -> Result<Self> {
        let manager = ConnectionManager::<SqliteConnection>::new(database);

        let pool = Pool::builder()
            .test_on_check_out(true)
            .build(manager)
            .map_err(|e| anyhow::anyhow!("Failed to create connection pool: {}", e))?;

        let connection = &mut pool.get().unwrap();
        Self::setup(connection)?;

        Ok(Self { pool: pool })
    }

    pub async fn new_in_memory() -> Result<Self> {
        let db_url = format!("file::memory:?mode=memory");
        let manager = ConnectionManager::<SqliteConnection>::new(db_url);

        let pool = Pool::builder()
            .test_on_check_out(true)
            .build(manager)
            .map_err(|e| anyhow::anyhow!("Failed to create connection pool: {}", e))?;

        let connection = &mut pool.get().unwrap();
        connection
            .batch_execute(include_str!("../../sql/create_tables.sql"))
            .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))?;

        Self::setup(connection)?;

        Ok(Self { pool: pool })
    }

    pub const TEST_INPUT_A: &'static str = "test_input_a.sql";
    pub const TEST_INPUT_B: &'static str = "test_input_b.sql";

    pub async fn load_test_input(&self, input_path: &str) -> Result<()> {
        let connection = &mut self.pool.get().unwrap();

        match input_path {
            "test_input_a.sql" => {
                connection
                    .batch_execute(include_str!("../../sql/test_input_a.sql"))
                    .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))
                    .unwrap();
            }
            "test_input_b.sql" => {
                connection
                    .batch_execute(include_str!("../../sql/test_input_b.sql"))
                    .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))
                    .unwrap();
            }
            "verb_test.sql" => {
                connection
                    .batch_execute(include_str!("../../sql/verb_test.sql"))
                    .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))
                    .unwrap();
            }
            _ => panic!("Impossible input file"),
        };

        Self::setup(connection)?;

        Ok(())
    }

    pub async fn get_file_contents(&self, file_id: FileId) -> Result<String> {
        use crate::schema_diesel::*;

        let connection = &mut self
            .pool
            .get()
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let file_id: i32 = file_id.into();
        let result = file_contents::dsl::file_contents
            .filter(file_contents::dsl::file_id.eq(file_id))
            .select(file_contents::dsl::content)
            .first::<Vec<u8>>(connection)
            .optional()
            .map_err(|e| anyhow::anyhow!("Failed to query file contents: {}", e))?;

        if result.is_none() {
            return Err(anyhow::anyhow!(
                "File contents not found for file_id {}",
                file_id
            ));
        }

        Ok(String::from_utf8_lossy(&result.unwrap()).to_string())
    }

    pub async fn find_symbol_by_name(&self, compound_name: &[&str]) -> Result<Selection> {
        use crate::schema_diesel::modules::dsl::*;
        use crate::schema_diesel::*;

        let fts_name_pattern = compound_name.join(" AND ");
        let name_pattern = compound_name.join("*");
        let name_pattern = format!("*{}*", name_pattern);

        let connection = &mut self
            .pool
            .get()
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let matched_symbols_query =
            sql_query("SELECT rowid FROM symbols_fts WHERE symbols_fts MATCH ?")
                .bind::<Text, _>(&fts_name_pattern);

        println!(
            "Executing FTS query: {:?}",
            debug_query::<Sqlite, _>(&matched_symbols_query)
        );

        let matched_symbols = matched_symbols_query
            .load::<SymbolRowid>(connection)
            .map_err(|e| anyhow::anyhow!("Failed to query FTS table: {}", e))?;

        println!("Matched {} symbols", matched_symbols.len());

        println!("Searching for symbols with name pattern: {fts_name_pattern}");
        use std::time::Instant;
        let now = Instant::now();
        let current = symbols::dsl::symbols
            .inner_join(declarations::dsl::declarations)
            .inner_join(modules)
            .inner_join(files::dsl::files.on(files::id.eq(declarations::file_id)))
            .select((
                Symbol::as_select(),
                Declaration::as_select(),
                Module::as_select(),
                File::as_select(),
            ))
            .filter(
                symbols::dsl::id.eq_any(
                    matched_symbols
                        .iter()
                        .map(|s| s.rowid)
                        .collect::<Vec<i32>>(),
                ),
            )
            .filter(symbols::dsl::name.glob(&name_pattern))
            .load::<(Symbol, Declaration, Module, File)>(connection)
            .map_err(|e| anyhow::anyhow!("Failed to load symbols: {}", e))?;

        let elapsed_current = now.elapsed();
        let (children_symbols, children_decls) =
            diesel::alias!(symbols as children_symbols, declarations as children_decls);

        let parents = symbol_refs::dsl::symbol_refs
            .inner_join(
                declarations::dsl::declarations
                    .on(symbol_refs::dsl::from_decl.eq(declarations::id)),
            )
            .inner_join(symbols::dsl::symbols.on(declarations::dsl::symbol.eq(symbols::id)))
            .inner_join(files::dsl::files.on(files::id.eq(declarations::file_id)))
            .inner_join(
                children_symbols.on(children_symbols
                    .field(symbols::id)
                    .eq(symbol_refs::dsl::to_symbol)),
            )
            .inner_join(
                children_decls.on(children_decls
                    .field(declarations::symbol)
                    .eq(children_symbols.field(symbols::id))),
            )
            .select((
                File::as_select(),
                Symbol::as_select(),
                Declaration::as_select(),
                SymbolRef::as_select(),
                children_symbols.default_selection(),
                children_decls.default_selection(),
            ))
            .filter(
                children_symbols.field(symbols::id).eq_any(
                    matched_symbols
                        .iter()
                        .map(|s| s.rowid)
                        .collect::<Vec<i32>>(),
                ),
            )
            .filter(children_symbols.field(symbols::name).glob(&name_pattern))
            .load::<(File, Symbol, Declaration, SymbolRef, Symbol, Declaration)>(connection)
            .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?;

        let elapsed_parents = now.elapsed();

        let (parent_symbols, parent_decls) =
            diesel::alias!(symbols as parent_symbols, declarations as parent_decls);

        let children = symbol_refs::dsl::symbol_refs
            .inner_join(symbols::dsl::symbols.on(symbol_refs::dsl::to_symbol.eq(symbols::id)))
            .inner_join(
                declarations::dsl::declarations.on(symbols::dsl::id.eq(declarations::symbol)),
            )
            .inner_join(
                parent_decls.on(parent_decls
                    .field(declarations::id)
                    .eq(symbol_refs::dsl::from_decl)),
            )
            .inner_join(
                parent_symbols.on(parent_symbols
                    .field(symbols::id)
                    .eq(parent_decls.field(declarations::symbol))),
            )
            .filter(
                parent_symbols.field(symbols::id).eq_any(
                    matched_symbols
                        .iter()
                        .map(|s| s.rowid)
                        .collect::<Vec<i32>>(),
                ),
            )
            .inner_join(
                files::dsl::files.on(files::dsl::id.eq(parent_decls.field(declarations::file_id))),
            )
            .filter(parent_symbols.field(symbols::name).glob(&name_pattern))
            .select((
                Symbol::as_select(),
                Declaration::as_select(),
                SymbolRef::as_select(),
                File::as_select(),
            ))
            .load::<(Symbol, Declaration, SymbolRef, File)>(connection)
            .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?;

        let elapsed_children = now.elapsed();

        let nodes: Vec<_> = current
            .into_iter()
            .map(|(sym, decl, module, file)| SelectionNode {
                symbol: sym,
                declaration: decl,
                module: module,
                file: file,
            })
            .collect();

        let parents: Vec<_> = parents
            .into_iter()
            .map(
                |(
                    from_file,
                    from_symbol,
                    from_declaration,
                    symbol_ref,
                    to_symbol,
                    to_declaration,
                )| ParentReference {
                    from_file,
                    from_symbol,
                    from_declaration,
                    symbol_ref,
                    to_symbol,
                    to_declaration,
                },
            )
            .collect();

        let children: Vec<_> = children
            .into_iter()
            .map(|(sym, decl, sym_ref, from_file)| ChildReference {
                symbol: sym,
                declaration: decl,
                symbol_ref: sym_ref,
                from_file,
            })
            .collect();

        let elapsed_collect = now.elapsed();

        println!(
            "Found {} current, {} parents, {} children",
            nodes.len(),
            parents.len(),
            children.len()
        );
        println!(
            "Query times: current: {:?} parents: {:?}, children: {:?}, collect: {:?}",
            elapsed_current,
            elapsed_parents - elapsed_current,
            elapsed_children - elapsed_parents,
            elapsed_collect - elapsed_children
        );

        Ok(Selection {
            nodes,
            parents,
            children,
        })
    }

    pub async fn find_symbol_by_declid(
        &self,
        declarations: &Vec<DeclarationId>,
    ) -> Result<Selection> {
        use crate::schema_diesel::modules::dsl::*;
        use crate::schema_diesel::*;

        let declarations: Vec<i32> = declarations.iter().map(|d| d.to_owned().into()).collect();

        let connection = &mut self
            .pool
            .get()
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        println!("Searching for symbols with by decl_id: {declarations:?}");
        let current = symbols::dsl::symbols
            .inner_join(declarations::dsl::declarations)
            .inner_join(modules)
            .inner_join(files::dsl::files.on(files::id.eq(declarations::file_id)))
            .filter(declarations::dsl::id.eq_any(&declarations))
            .select((
                Symbol::as_select(),
                Declaration::as_select(),
                Module::as_select(),
                File::as_select(),
            ))
            .load::<(Symbol, Declaration, Module, File)>(connection)
            .map_err(|e| anyhow::anyhow!("Failed to load symbols: {}", e))?;

        let (children_symbols, children_decls) =
            diesel::alias!(symbols as children_symbols, declarations as children_decls);

        let parents = symbol_refs::dsl::symbol_refs
            .inner_join(
                declarations::dsl::declarations
                    .on(symbol_refs::dsl::from_decl.eq(declarations::id)),
            )
            .inner_join(symbols::dsl::symbols.on(declarations::dsl::symbol.eq(symbols::id)))
            .inner_join(files::dsl::files.on(files::id.eq(declarations::file_id)))
            .inner_join(
                children_symbols.on(children_symbols
                    .field(symbols::id)
                    .eq(symbol_refs::dsl::to_symbol)),
            )
            .inner_join(
                children_decls.on(children_decls
                    .field(declarations::symbol)
                    .eq(children_symbols.field(symbols::id))),
            )
            .select((
                File::as_select(),
                Symbol::as_select(),
                Declaration::as_select(),
                SymbolRef::as_select(),
                children_symbols.default_selection(),
                children_decls.default_selection(),
            ))
            .filter(children_decls.field(declarations::id).eq_any(&declarations))
            .load::<(File, Symbol, Declaration, SymbolRef, Symbol, Declaration)>(connection)
            .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?;

        let (parent_symbols, parent_decls) =
            diesel::alias!(symbols as parent_symbols, declarations as parent_decls);

        let children = symbol_refs::dsl::symbol_refs
            .inner_join(symbols::dsl::symbols.on(symbol_refs::dsl::to_symbol.eq(symbols::id)))
            .inner_join(
                declarations::dsl::declarations.on(symbols::dsl::id.eq(declarations::symbol)),
            )
            .inner_join(
                parent_decls.on(parent_decls
                    .field(declarations::id)
                    .eq(symbol_refs::dsl::from_decl)),
            )
            .inner_join(
                parent_symbols.on(parent_symbols
                    .field(symbols::id)
                    .eq(parent_decls.field(declarations::symbol))),
            )
            .inner_join(
                files::dsl::files.on(files::dsl::id.eq(parent_decls.field(declarations::file_id))),
            )
            .select((
                Symbol::as_select(),
                Declaration::as_select(),
                SymbolRef::as_select(),
                File::as_select(),
            ))
            .filter(parent_decls.field(declarations::id).eq_any(&declarations))
            .load::<(Symbol, Declaration, SymbolRef, File)>(connection)
            .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?;

        let current = current
            .into_iter()
            .map(|(sym, decl, module, file)| SelectionNode {
                symbol: sym,
                declaration: decl,
                module: module,
                file: file,
            })
            .collect();

        let parents = parents
            .into_iter()
            .map(
                |(
                    from_file,
                    from_symbol,
                    from_declaration,
                    symbol_ref,
                    to_symbol,
                    to_declaration,
                )| ParentReference {
                    from_file,
                    from_symbol,
                    from_declaration,
                    symbol_ref,
                    to_symbol,
                    to_declaration,
                },
            )
            .collect();

        let children = children
            .into_iter()
            .map(|(sym, decl, sym_ref, from_file)| ChildReference {
                symbol: sym,
                declaration: decl,
                symbol_ref: sym_ref,
                from_file,
            })
            .collect();

        Ok(Selection {
            nodes: current,
            parents,
            children,
        })
    }
}
