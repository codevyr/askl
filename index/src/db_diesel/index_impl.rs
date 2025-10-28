use anyhow::Result;
use diesel::connection::SimpleConnection;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::sqlite::Sqlite;
use diesel::SqliteConnection;
use diesel_migrations::MigrationHarness;

use crate::models_diesel::{Declaration, File, Module, Symbol, SymbolRef};
use crate::symbols::{DeclarationId, FileId};

use super::mixins::{
    CompoundNameMixin, DeclarationIdMixin, SymbolSearchMixin, CHILDREN_SYMBOLS_ALIAS,
    PARENT_DECLS_ALIAS, PARENT_SYMBOLS_ALIAS,
};
use super::selection::{ChildReference, ParentReference, Selection, SelectionNode};
use super::Connection;

pub struct Index {
    pub(super) pool: Pool<ConnectionManager<SqliteConnection>>,
}

impl Index {
    fn setup(connection: &mut Connection) -> Result<()> {
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
        connection
            .run_pending_migrations(super::MIGRATIONS)
            .unwrap();

        Self::setup(connection)?;

        Ok(Self { pool })
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
            .batch_execute(include_str!("../../../sql/create_tables.sql"))
            .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))?;

        connection
            .run_pending_migrations(super::MIGRATIONS)
            .unwrap();
        Self::setup(connection)?;

        Ok(Self { pool })
    }

    pub const TEST_INPUT_A: &'static str = "test_input_a.sql";
    pub const TEST_INPUT_B: &'static str = "test_input_b.sql";
    pub const TEST_INPUT_MODULES: &'static str = "test_input_modules.sql";

    pub async fn load_test_input(&self, input_path: &str) -> Result<()> {
        let connection = &mut self.pool.get().unwrap();

        connection.revert_all_migrations(super::MIGRATIONS).unwrap();
        println!(
            "Has pending migrations: {}",
            connection.has_pending_migration(super::MIGRATIONS).unwrap()
        );
        connection
            .run_pending_migrations(super::MIGRATIONS)
            .unwrap();

        match input_path {
            "test_input_a.sql" => {
                connection
                    .batch_execute(include_str!("../../../sql/test_input_a.sql"))
                    .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))
                    .unwrap();
            }
            "test_input_b.sql" => {
                connection
                    .batch_execute(include_str!("../../../sql/test_input_b.sql"))
                    .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))
                    .unwrap();
            }
            "test_input_modules.sql" => {
                connection
                    .batch_execute(include_str!("../../../sql/test_input_modules.sql"))
                    .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))
                    .unwrap();
            }
            "verb_test.sql" => {
                connection
                    .batch_execute(include_str!("../../../sql/verb_test.sql"))
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

    pub async fn find_symbol(
        &self,
        mixins: &mut [Box<dyn SymbolSearchMixin>],
    ) -> Result<Selection> {
        use crate::schema_diesel::modules::dsl::*;
        use crate::schema_diesel::*;

        let connection: &mut Connection = &mut self
            .pool
            .get()
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        for mixin in mixins.iter_mut() {
            mixin.enter(connection)?;
        }

        let current = {
            let _select_current: tracing::span::EnteredSpan =
                tracing::info_span!("select_current").entered();

            let mut joined_query = symbols::dsl::symbols
                .inner_join(declarations::dsl::declarations)
                .inner_join(modules)
                .inner_join(files::dsl::files.on(files::id.eq(declarations::file_id)))
                .select((
                    Symbol::as_select(),
                    Declaration::as_select(),
                    Module::as_select(),
                    File::as_select(),
                ))
                .into_boxed::<Sqlite>();

            for mixin in mixins.iter_mut() {
                joined_query = mixin.filter_current(connection, joined_query)?;
            }

            joined_query
                .load::<(Symbol, Declaration, Module, File)>(connection)
                .map_err(|e| anyhow::anyhow!("Failed to load symbols: {}", e))?
        };

        let parent_decls = PARENT_DECLS_ALIAS;
        let parent_symbols = PARENT_SYMBOLS_ALIAS;

        let parents = {
            let _parents_span: tracing::span::EnteredSpan =
                tracing::info_span!("select_parents").entered();
            let mut parents_query = symbol_refs::dsl::symbol_refs
                .inner_join(
                    CHILDREN_SYMBOLS_ALIAS.on(CHILDREN_SYMBOLS_ALIAS
                        .field(symbols::dsl::id)
                        .eq(symbol_refs::dsl::to_symbol)),
                )
                .inner_join(
                    symbols::dsl::symbols.on(symbol_refs::dsl::to_symbol.eq(symbols::dsl::id)),
                )
                .inner_join(
                    declarations::dsl::declarations
                        .on(symbols::dsl::id.eq(declarations::dsl::symbol)),
                )
                .inner_join(
                    parent_decls.on(parent_decls
                        .field(declarations::dsl::id)
                        .eq(symbol_refs::dsl::from_decl)),
                )
                .select((
                    SymbolRef::as_select(),
                    Symbol::as_select(),
                    Declaration::as_select(),
                    parent_decls.fields(crate::schema_diesel::declarations::all_columns),
                ))
                .into_boxed::<Sqlite>();

            for mixin in mixins.iter_mut() {
                parents_query = mixin.filter_parents(connection, parents_query)?;
            }

            parents_query
                .load::<(SymbolRef, Symbol, Declaration, Declaration)>(connection)
                .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?
        };

        let children = {
            let _select_children: tracing::span::EnteredSpan =
                tracing::info_span!("select_children").entered();

            let mut children_query = symbol_refs::dsl::symbol_refs
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
                    files::dsl::files
                        .on(files::dsl::id.eq(parent_decls.field(declarations::file_id))),
                )
                .select((
                    parent_symbols.fields(crate::schema_diesel::symbols::all_columns),
                    Symbol::as_select(),
                    Declaration::as_select(),
                    SymbolRef::as_select(),
                    File::as_select(),
                ))
                .into_boxed::<Sqlite>();

            for mixin in mixins {
                children_query = mixin.filter_children(connection, children_query)?;
            }

            children_query
                .load::<(Symbol, Symbol, Declaration, SymbolRef, File)>(connection)
                .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?
        };

        let selection = {
            let _collect_span: tracing::span::EnteredSpan =
                tracing::info_span!("collect").entered();

            let nodes: Vec<_> = current
                .into_iter()
                .map(|(sym, decl, module, file)| SelectionNode {
                    symbol: sym,
                    declaration: decl,
                    module,
                    file,
                })
                .collect();

            let parents: Vec<_> = parents
                .into_iter()
                .map(
                    |(symbol_ref, to_symbol, to_declaration, from_declaration)| ParentReference {
                        symbol_ref,
                        to_symbol,
                        to_declaration,
                        from_declaration,
                    },
                )
                .collect();

            let mut children: Vec<_> = children
                .into_iter()
                .map(
                    |(parent_symbol, sym, decl, sym_ref, from_file)| ChildReference {
                        parent_symbol,
                        symbol: sym,
                        declaration: decl,
                        symbol_ref: sym_ref,
                        from_file,
                    },
                )
                .collect();

            children.sort_by_key(|child| (child.symbol_ref.from_decl, child.declaration.id));

            println!(
                "Found {} current, {} parents, {} children",
                nodes.len(),
                parents.len(),
                children.len()
            );

            Ok(Selection {
                nodes,
                parents,
                children,
            })
        };

        selection
    }

    pub async fn find_symbol_by_name(&self, name: &str) -> Result<Selection> {
        let mixin = CompoundNameMixin::new(name);
        let mut mixins: Vec<Box<dyn SymbolSearchMixin>> = vec![Box::new(mixin)];
        self.find_symbol(&mut mixins).await
    }

    pub async fn find_symbol_by_declid(
        &self,
        declarations: &Vec<DeclarationId>,
    ) -> Result<Selection> {
        let mixin = DeclarationIdMixin::new(declarations);
        let mut mixins: Vec<Box<dyn SymbolSearchMixin>> = vec![Box::new(mixin)];
        self.find_symbol(&mut mixins).await
    }
}
