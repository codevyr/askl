use anyhow::Result;
use diesel::connection::SimpleConnection;
use diesel::pg::{Pg, PgConnection};
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::PgRangeExpressionMethods;
use diesel_migrations::MigrationHarness;

use crate::models_diesel::{Object, Project, Symbol, SymbolInstance, SymbolRef};
use crate::symbols::FileId;

use super::mixins::{
    CompoundNameMixin, SymbolSearchMixin, PARENT_DECLS_ALIAS, PARENT_SYMBOLS_ALIAS,
};
use super::selection::{ChildReference, ParentReference, Selection, SelectionNode};
use super::Connection;

pub struct Index {
    pub(super) pool: Pool<ConnectionManager<PgConnection>>,
}

impl Index {
    pub fn from_pool(pool: Pool<ConnectionManager<PgConnection>>) -> Result<Self> {
        let connection = &mut pool
            .get()
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;
        connection
            .run_pending_migrations(super::MIGRATIONS)
            .map_err(|e| anyhow::anyhow!("Failed to run migrations: {}", e))?;
        Ok(Self { pool })
    }

    pub async fn connect(database_url: &str) -> Result<Self> {
        let manager = ConnectionManager::<PgConnection>::new(database_url);
        let pool = Pool::builder()
            .test_on_check_out(true)
            .build(manager)
            .map_err(|e| anyhow::anyhow!("Failed to create connection pool: {}", e))?;

        Self::from_pool(pool)
    }

    pub async fn new_in_memory() -> Result<Self> {
        Err(anyhow::anyhow!(
            "In-memory Postgres is not supported; use Index::connect with a test database"
        ))
    }

    pub const TEST_INPUT_A: &'static str = "test_input_a.sql";
    pub const TEST_INPUT_B: &'static str = "test_input_b.sql";
    pub const TEST_INPUT_MODULES: &'static str = "test_input_modules.sql";
    pub const TEST_INPUT_SYMBOL_TOKENS: &'static str = "test_input_symbol_tokens.sql";

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
            "test_input_symbol_tokens.sql" => {
                connection
                    .batch_execute(include_str!("../../../sql/test_input_symbol_tokens.sql"))
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

        Ok(())
    }

    pub async fn get_file_contents(&self, object_id: FileId) -> Result<String> {
        use crate::schema_diesel::*;

        let connection = &mut self
            .pool
            .get()
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let object_id: i32 = object_id.into();
        let result = object_contents::dsl::object_contents
            .filter(object_contents::dsl::object_id.eq(object_id))
            .select(object_contents::dsl::content)
            .first::<Vec<u8>>(connection)
            .optional()
            .map_err(|e| anyhow::anyhow!("Failed to query file contents: {}", e))?;

        if result.is_none() {
            return Err(anyhow::anyhow!(
                "File contents not found for object_id {}",
                object_id
            ));
        }

        Ok(String::from_utf8_lossy(&result.unwrap()).to_string())
    }

    pub async fn find_symbol(
        &self,
        mixins: &mut [Box<dyn SymbolSearchMixin>],
    ) -> Result<Selection> {
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
                .inner_join(
                    symbol_instances::dsl::symbol_instances
                        .on(symbols::dsl::id.eq(symbol_instances::dsl::symbol)),
                )
                .inner_join(
                    projects::dsl::projects.on(symbols::dsl::project_id.eq(projects::dsl::id)),
                )
                .inner_join(objects::dsl::objects.on(objects::dsl::id.eq(symbol_instances::dsl::object_id)))
                .select((
                    Symbol::as_select(),
                    SymbolInstance::as_select(),
                    Object::as_select(),
                    Project::as_select(),
                ))
                .into_boxed::<Pg>();

            for mixin in mixins.iter_mut() {
                joined_query = mixin.filter_current(connection, joined_query)?;
            }

            joined_query
                .load::<(Symbol, SymbolInstance, Object, Project)>(connection)
                .map_err(|e| anyhow::anyhow!("Failed to load symbols: {}", e))?
        };

        let parent_decls = PARENT_DECLS_ALIAS;
        let parent_symbols = PARENT_SYMBOLS_ALIAS;

        let parents = {
            let _parents_span: tracing::span::EnteredSpan =
                tracing::info_span!("select_parents").entered();
            let mut parents_query = symbol_refs::dsl::symbol_refs
                .inner_join(
                    symbols::dsl::symbols.on(symbol_refs::dsl::to_symbol.eq(symbols::dsl::id)),
                )
                .inner_join(
                    symbol_instances::dsl::symbol_instances
                        .on(symbols::dsl::id.eq(symbol_instances::dsl::symbol)),
                )
                .inner_join(
                    parent_decls.on(parent_decls
                        .field(symbol_instances::dsl::object_id)
                        .eq(symbol_refs::dsl::from_object)),
                )
                .filter(
                    parent_decls
                        .field(symbol_instances::dsl::offset_range)
                        .contains_range(symbol_refs::dsl::from_offset_range),
                )
                .select((
                    SymbolRef::as_select(),
                    Symbol::as_select(),
                    SymbolInstance::as_select(),
                    parent_decls.fields(crate::schema_diesel::symbol_instances::all_columns),
                ))
                .into_boxed::<Pg>();

            for mixin in mixins.iter_mut() {
                parents_query = mixin.filter_parents(connection, parents_query)?;
            }

            parents_query
                .load::<(SymbolRef, Symbol, SymbolInstance, SymbolInstance)>(connection)
                .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?
        };

        let children = {
            let _select_children: tracing::span::EnteredSpan =
                tracing::info_span!("select_children").entered();

            let mut children_query = symbol_refs::dsl::symbol_refs
                .inner_join(symbols::dsl::symbols.on(symbol_refs::dsl::to_symbol.eq(symbols::id)))
                .inner_join(
                    symbol_instances::dsl::symbol_instances.on(symbols::dsl::id.eq(symbol_instances::symbol)),
                )
                .inner_join(
                    parent_decls.on(parent_decls
                        .field(symbol_instances::dsl::object_id)
                        .eq(symbol_refs::dsl::from_object)),
                )
                .filter(
                    parent_decls
                        .field(symbol_instances::dsl::offset_range)
                        .contains_range(symbol_refs::dsl::from_offset_range),
                )
                .inner_join(
                    parent_symbols.on(parent_symbols
                        .field(symbols::dsl::id)
                        .eq(parent_decls.field(symbol_instances::dsl::symbol))),
                )
                .inner_join(
                    objects::dsl::objects
                        .on(objects::dsl::id.eq(parent_decls.field(symbol_instances::dsl::object_id))),
                )
                .select((
                    parent_symbols.fields(crate::schema_diesel::symbols::all_columns),
                    Symbol::as_select(),
                    SymbolInstance::as_select(),
                    parent_decls.fields(crate::schema_diesel::symbol_instances::all_columns),
                    SymbolRef::as_select(),
                    Object::as_select(),
                ))
                .into_boxed::<Pg>();

            for mixin in mixins {
                children_query = mixin.filter_children(connection, children_query)?;
            }

            children_query
                .load::<(Symbol, Symbol, SymbolInstance, SymbolInstance, SymbolRef, Object)>(connection)
                .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?
        };

        let selection = {
            let _collect_span: tracing::span::EnteredSpan =
                tracing::info_span!("collect").entered();

            let nodes: Vec<_> = current
                .into_iter()
                .map(|(sym, instance, object, project)| SelectionNode {
                    symbol: sym,
                    symbol_instance: instance,
                    object,
                    project,
                })
                .collect();

            let parents: Vec<_> = parents
                .into_iter()
                .map(
                    |(symbol_ref, to_symbol, to_instance, from_instance)| ParentReference {
                        symbol_ref,
                        to_symbol,
                        to_instance,
                        from_instance,
                    },
                )
                .collect();

            let mut children: Vec<_> = children
                .into_iter()
                .map(
                    |(parent_symbol, sym, instance, from_instance, sym_ref, from_object)| {
                        ChildReference {
                            parent_symbol,
                            symbol: sym,
                            symbol_instance: instance,
                            from_instance,
                            symbol_ref: sym_ref,
                            from_object,
                        }
                    },
                )
                .collect();

            children.sort_by_key(|child| (child.from_instance.id, child.symbol_instance.id));

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
}
