use diesel_async::AsyncPgConnection;
use diesel_migrations::{embed_migrations, EmbeddedMigrations};

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("../migrations");

mod cte;
mod index_impl;
pub(crate) mod mixins;
mod selection;
mod sql_cache;
pub mod stats;

pub use index_impl::{
    eph_pool_manager_config, layer_shard_hash, purge_eph_cache, selection_shard_hash,
    EphInstanceRow, EphLayerKind, EphLayerMeta, EphRefRow, EphScopedFut, EphSymbolRow,
    EphTransaction, ImplicitEdge, Index, LayerBatch, LayerOutcome, MaterialisedLayer,
    NameSuggestionRow, ProbeOutcome, ProbeRole, ResolvedSourceFile, RootShardRef, ScopeContext,
    SearchMatchRow, ShardRole, DEFAULT_SQL_CACHE_BYTES, EPH_POOL_IDLE_IN_TXN_TIMEOUT,
    EPH_POOL_RECYCLING_QUERY,
};
pub use mixins::{
    CompositeFilter, CompoundNameMixin, CurrentQuery, DefaultSymbolTypeMixin, DirectOnlyMixin,
    ExactNameMixin, FilterLeaf, FullNameGlobMixin, GlobNameMixin, GlobPiece, InnermostOnlyMixin,
    LeafNameMixin, OuterParentFilterMixin, PackageDescendantLeaf, ProjectFilterMixin,
    SymbolInstanceIdMixin, SymbolTypeMixin, INSTANCE_TYPE_BUILD, INSTANCE_TYPE_CONTAINMENT,
    INSTANCE_TYPE_DECLARATION, INSTANCE_TYPE_DEFINITION, INSTANCE_TYPE_DOCUMENTATION,
    INSTANCE_TYPE_EXPANSION, INSTANCE_TYPE_FILE, INSTANCE_TYPE_HEADER, INSTANCE_TYPE_SENTINEL,
    INSTANCE_TYPE_SOURCE, SYMBOL_TYPE_CONTENT, SYMBOL_TYPE_DATA, SYMBOL_TYPE_DIRECTORY,
    SYMBOL_TYPE_FIELD, SYMBOL_TYPE_FILE, SYMBOL_TYPE_FUNCTION, SYMBOL_TYPE_MACRO,
    SYMBOL_TYPE_MODULE, SYMBOL_TYPE_TYPE,
};
pub use selection::{
    Checked, ChildReference, EphContext, HasChildReference, HasEphLeak, HasParentReference,
    ObjectFullDiesel, ParentReference, QueryStatementRange, ReferenceFullDiesel, ReferenceResult,
    ResultBudget, RootLayer, Selection, SelectionNode, SymbolInstanceFullDiesel, CANARY_LAYER_ID,
};
pub use sql_cache::{
    vec_weight, CacheKey, CacheStats, CacheWeight, ClearOnDrop, RowKey, SqlResultCache,
};
pub use stats::{
    analyze_index_schema, ensure_planner_stats, index_table_names, index_table_stats_health,
    log_report, AnalyzeReport, BootAnalyze, StatsStaleness, TableStatsHealth,
};

pub type Connection = AsyncPgConnection;
