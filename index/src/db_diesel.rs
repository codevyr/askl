use diesel::pg::PgConnection;
use diesel::r2d2::{ConnectionManager, PooledConnection};
use diesel_migrations::{embed_migrations, EmbeddedMigrations};

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("../migrations");

mod index_impl;
pub mod mixins;
mod selection;

pub use index_impl::Index;
pub use mixins::{
    CompoundNameMixin, CurrentQuery, DeclarationIdMixin, IgnoreFilterMixin,
    ProjectFilterMixin, SymbolSearchMixin,
};
pub use selection::{
    ChildReference, HasChildReference, HasParentReference, ObjectFullDiesel, ParentReference,
    ReferenceFullDiesel, ReferenceResult, Selection, SelectionNode, SymbolInstanceFullDiesel,
};

pub type Connection = PooledConnection<ConnectionManager<PgConnection>>;
