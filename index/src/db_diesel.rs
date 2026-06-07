use diesel_async::AsyncPgConnection;
use diesel_migrations::{embed_migrations, EmbeddedMigrations};

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("../migrations");

mod index_impl;
pub(crate) mod mixins;
mod selection;

pub use index_impl::{EphInstanceRow, EphLayerKind, EphRefRow, EphScopedFut, EphSymbolRow, EphTransaction, ImplicitEdge, Index, LayerBatch, ScopeContext, purge_eph_cache, EPH_POOL_RECYCLING_QUERY};
pub use mixins::{
    CompositeFilter, CompoundNameMixin, CurrentQuery, DefaultSymbolTypeMixin,
    DirectOnlyMixin, ExactNameMixin, FilterLeaf, InnermostOnlyMixin,
    LeafNameMixin, OuterParentFilterMixin, PackageDescendantLeaf,
    ProjectFilterMixin, SymbolInstanceIdMixin,
    SymbolTypeMixin,
    SYMBOL_TYPE_FUNCTION, SYMBOL_TYPE_FILE, SYMBOL_TYPE_MODULE, SYMBOL_TYPE_DIRECTORY, SYMBOL_TYPE_TYPE, SYMBOL_TYPE_DATA, SYMBOL_TYPE_MACRO, SYMBOL_TYPE_FIELD,
    INSTANCE_TYPE_DEFINITION, INSTANCE_TYPE_DECLARATION, INSTANCE_TYPE_EXPANSION, INSTANCE_TYPE_SENTINEL, INSTANCE_TYPE_CONTAINMENT, INSTANCE_TYPE_SOURCE, INSTANCE_TYPE_HEADER, INSTANCE_TYPE_BUILD, INSTANCE_TYPE_FILE, INSTANCE_TYPE_DOCUMENTATION,
};
pub use selection::{
    Checked, ChildReference, EphContext, HasChildReference, HasEphLeak, HasParentReference,
    ObjectFullDiesel, ParentReference, QueryStatementRange, ReferenceFullDiesel, ReferenceResult,
    Selection, SelectionNode, SymbolInstanceFullDiesel, CANARY_LAYER_ID,
};

pub type Connection = AsyncPgConnection;
