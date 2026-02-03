use diesel::pg::PgConnection;
use diesel::r2d2::{ConnectionManager, PooledConnection};
use diesel_migrations::{embed_migrations, EmbeddedMigrations};

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("../migrations");

mod index_impl;
mod mixins;
mod selection;

pub use index_impl::Index;
pub use mixins::{
    CompoundNameMixin, DeclarationIdMixin, ModuleFilterMixin, ProjectFilterMixin, SymbolSearchMixin,
};
pub use selection::{
    ChildReference, DeclarationFullDiesel, FileFullDiesel, ModuleFullDiesel, ParentReference,
    ReferenceFullDiesel, ReferenceResult, Selection, SelectionNode,
};

pub type Connection = PooledConnection<ConnectionManager<PgConnection>>;
