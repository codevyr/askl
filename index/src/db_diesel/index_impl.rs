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
    SymbolSearchMixin, PARENT_DECLS_ALIAS, PARENT_SYMBOLS_ALIAS,
    CONTAINER_INSTANCE_ALIAS, CONTAINER_SYMBOL_ALIAS, CONTAINER_TYPE_ALIAS,
    CONTAINED_INSTANCE_ALIAS, CONTAINED_SYMBOL_ALIAS, CONTAINED_TYPE_ALIAS,
};
use super::selection::{ChildReference, HasChildReference, HasParentReference, ParentReference, Selection, SelectionNode};
use super::Connection;

#[derive(Clone)]
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
    pub const TEST_INPUT_CONTAINMENT: &'static str = "test_input_containment.sql";
    pub const TEST_INPUT_TREE_BROWSER: &'static str = "test_input_tree_browser.sql";
    pub const TEST_INPUT_NESTED_FUNC: &'static str = "test_input_nested_func.sql";
    pub const VERB_TEST: &'static str = "verb_test.sql";

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
            "test_input_containment.sql" => {
                connection
                    .batch_execute(include_str!("../../../sql/test_input_containment.sql"))
                    .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))
                    .unwrap();
            }
            "test_input_tree_browser.sql" => {
                connection
                    .batch_execute(include_str!("../../../sql/test_input_tree_browser.sql"))
                    .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))
                    .unwrap();
            }
            "test_input_nested_func.sql" => {
                connection
                    .batch_execute(include_str!("../../../sql/test_input_nested_func.sql"))
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
                // Join the parent's symbol (caller) to enable type filtering via mixin
                .inner_join(
                    parent_symbols.on(parent_symbols
                        .field(symbols::dsl::id)
                        .eq(parent_decls.field(symbol_instances::dsl::symbol))),
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
                // Join the parent's symbol (caller) to enable type filtering via mixin
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

            for mixin in mixins.iter_mut() {
                children_query = mixin.filter_children(connection, children_query)?;
            }

            children_query
                .load::<(Symbol, Symbol, SymbolInstance, SymbolInstance, SymbolRef, Object)>(connection)
                .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?
        };

        // Query for containment: find containers (parents that contain current symbols)
        // A container contains the current symbol if:
        // 1. They share the same object_id
        // 2. Container's offset_range @> current's offset_range
        // 3. Container's type level > current's type level
        let container_instance = CONTAINER_INSTANCE_ALIAS;
        let container_symbol = CONTAINER_SYMBOL_ALIAS;
        let container_type = CONTAINER_TYPE_ALIAS;

        let has_parents = {
            let _has_parents_span: tracing::span::EnteredSpan =
                tracing::info_span!("select_has_parents").entered();

            // For each current symbol instance, find container instances
            // that have: same object_id, container.offset_range @> current.offset_range,
            // container.type.level > current.type.level
            let mut has_parents_query = symbol_instances::dsl::symbol_instances
                .inner_join(symbols::dsl::symbols.on(symbol_instances::dsl::symbol.eq(symbols::dsl::id)))
                .inner_join(symbol_types::dsl::symbol_types.on(symbols::dsl::symbol_type.eq(symbol_types::dsl::id)))
                .inner_join(
                    container_instance.on(
                        container_instance.field(symbol_instances::dsl::object_id)
                            .eq(symbol_instances::dsl::object_id)
                    ),
                )
                .inner_join(
                    container_symbol.on(
                        container_symbol.field(symbols::dsl::id)
                            .eq(container_instance.field(symbol_instances::dsl::symbol))
                    ),
                )
                .inner_join(
                    container_type.on(
                        container_type.field(symbol_types::dsl::id)
                            .eq(container_symbol.field(symbols::dsl::symbol_type))
                    ),
                )
                .filter(
                    // Container's range contains current's range
                    diesel::dsl::sql::<diesel::sql_types::Bool>(
                        "container_instances.offset_range @> symbol_instances.offset_range"
                    )
                )
                .filter(
                    // Container's type level >= current's type level
                    // directory(4) > module(3) > file(2) > function(1)
                    // Using >= to support function-contains-function (nested/anonymous functions)
                    container_type.field(symbol_types::dsl::level)
                        .ge(symbol_types::dsl::level)
                )
                .filter(
                    // Prevent self-containment: container instance must differ from current instance
                    container_instance.field(symbol_instances::dsl::id)
                        .ne(symbol_instances::dsl::id)
                )
                .select((
                    Symbol::as_select(),                    // child_symbol (current)
                    SymbolInstance::as_select(),            // child_instance (current)
                    container_symbol.fields(crate::schema_diesel::symbols::all_columns),  // parent_symbol
                    container_instance.fields(crate::schema_diesel::symbol_instances::all_columns), // parent_instance
                ))
                .into_boxed::<Pg>();

            // Apply mixin filters to constrain to current symbols
            for mixin in mixins.iter_mut() {
                has_parents_query = mixin.filter_has_parents(connection, has_parents_query)?;
            }

            has_parents_query
                .load::<(Symbol, SymbolInstance, Symbol, SymbolInstance)>(connection)
                .map_err(|e| anyhow::anyhow!("Failed to load containment parents: {}", e))?
        };

        // Query for containment: find contained symbols (children that are contained by current symbols)
        let contained_instance = CONTAINED_INSTANCE_ALIAS;
        let contained_symbol = CONTAINED_SYMBOL_ALIAS;
        let contained_type = CONTAINED_TYPE_ALIAS;

        let has_children = {
            let _has_children_span: tracing::span::EnteredSpan =
                tracing::info_span!("select_has_children").entered();

            // For each current symbol instance (parent), find contained instances (children)
            // that have: same object_id, current.offset_range @> contained.offset_range,
            // current.type.level > contained.type.level
            let mut has_children_query = symbol_instances::dsl::symbol_instances
                .inner_join(symbols::dsl::symbols.on(symbol_instances::dsl::symbol.eq(symbols::dsl::id)))
                .inner_join(symbol_types::dsl::symbol_types.on(symbols::dsl::symbol_type.eq(symbol_types::dsl::id)))
                .inner_join(objects::dsl::objects.on(objects::dsl::id.eq(symbol_instances::dsl::object_id)))
                .inner_join(
                    contained_instance.on(
                        contained_instance.field(symbol_instances::dsl::object_id)
                            .eq(symbol_instances::dsl::object_id)
                    ),
                )
                .inner_join(
                    contained_symbol.on(
                        contained_symbol.field(symbols::dsl::id)
                            .eq(contained_instance.field(symbol_instances::dsl::symbol))
                    ),
                )
                .inner_join(
                    contained_type.on(
                        contained_type.field(symbol_types::dsl::id)
                            .eq(contained_symbol.field(symbols::dsl::symbol_type))
                    ),
                )
                .filter(
                    // Current's range contains contained's range
                    diesel::dsl::sql::<diesel::sql_types::Bool>(
                        "symbol_instances.offset_range @> contained_instances.offset_range"
                    )
                )
                .filter(
                    // Current's type level >= contained's type level
                    // Using >= to support function-contains-function (nested/anonymous functions)
                    symbol_types::dsl::level
                        .ge(contained_type.field(symbol_types::dsl::level))
                )
                .filter(
                    // Prevent self-containment: current instance must differ from contained instance
                    symbol_instances::dsl::id
                        .ne(contained_instance.field(symbol_instances::dsl::id))
                )
                .select((
                    Symbol::as_select(),                    // parent_symbol (current)
                    SymbolInstance::as_select(),            // parent_instance (current)
                    contained_symbol.fields(crate::schema_diesel::symbols::all_columns),  // child_symbol
                    contained_instance.fields(crate::schema_diesel::symbol_instances::all_columns), // child_instance
                    Object::as_select(),                    // parent_object
                ))
                .into_boxed::<Pg>();

            // Apply mixin filters to constrain to current symbols
            for mixin in mixins.iter_mut() {
                has_children_query = mixin.filter_has_children(connection, has_children_query)?;
            }

            has_children_query
                .load::<(Symbol, SymbolInstance, Symbol, SymbolInstance, Object)>(connection)
                .map_err(|e| anyhow::anyhow!("Failed to load containment children: {}", e))?
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

            let has_parents: Vec<_> = has_parents
                .into_iter()
                .map(|(child_symbol, child_instance, parent_symbol, parent_instance)| {
                    HasParentReference {
                        child_symbol,
                        child_instance,
                        parent_symbol,
                        parent_instance,
                    }
                })
                .collect();

            let mut has_children: Vec<_> = has_children
                .into_iter()
                .map(|(parent_symbol, parent_instance, child_symbol, child_instance, parent_object)| {
                    HasChildReference {
                        parent_symbol,
                        parent_instance,
                        child_symbol,
                        child_instance,
                        parent_object,
                    }
                })
                .collect();

            has_children.sort_by_key(|child| (child.parent_instance.id, child.child_instance.id));

            Ok(Selection {
                nodes,
                parents,
                children,
                has_parents,
                has_children,
            })
        };

        selection
    }

}
