use diesel::r2d2::{ConnectionManager, PooledConnection};
use diesel::SqliteConnection;
use diesel_migrations::{embed_migrations, EmbeddedMigrations};

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!();

mod index_impl;
mod mixins;
mod selection;

pub use index_impl::Index;
pub use mixins::{CompoundNameMixin, DeclarationIdMixin, ModuleFilterMixin, SymbolSearchMixin};
pub use selection::{
    ChildReference, DeclarationFullDiesel, FileFullDiesel, ModuleFullDiesel, ParentReference,
    ReferenceFullDiesel, ReferenceResult, Selection, SelectionNode,
};

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

pub type Connection = PooledConnection<ConnectionManager<SqliteConnection>>;
