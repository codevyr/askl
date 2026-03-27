use diesel::pg::PgConnection;
use diesel::r2d2::{ConnectionManager, PooledConnection};
use diesel_migrations::{embed_migrations, EmbeddedMigrations};

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("../migrations");

mod index_impl;
pub mod mixins;
mod selection;

pub use index_impl::Index;
pub use mixins::{
    CompoundNameMixin, CurrentQuery, SymbolInstanceIdMixin, ExactNameMixin,
    IgnoreFilterMixin, ProjectFilterMixin, SymbolSearchMixin,
    SYMBOL_TYPE_FUNCTION, SYMBOL_TYPE_FILE, SYMBOL_TYPE_MODULE, SYMBOL_TYPE_DIRECTORY, SYMBOL_TYPE_TYPE,
};
pub use selection::{
    ChildReference, HasChildReference, HasParentReference, ObjectFullDiesel, ParentReference,
    ReferenceFullDiesel, ReferenceResult, Selection, SelectionNode, SymbolInstanceFullDiesel,
};

pub type Connection = PooledConnection<ConnectionManager<PgConnection>>;
