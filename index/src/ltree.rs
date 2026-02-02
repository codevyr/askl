use diesel::backend::Backend;
use diesel::deserialize::{self, FromSql};
use diesel::pg::Pg;
use diesel::serialize::{self, Output, ToSql};
use diesel::sql_types::{SqlType, Text};
use diesel::{AsExpression, FromSqlRow};
#[derive(SqlType)]
#[diesel(postgres_type(name = "ltree"))]
pub struct Ltree;

#[derive(AsExpression, FromSqlRow, Debug, Clone)]
#[diesel(sql_type = Ltree)]
pub struct LtreePath(pub String);

impl ToSql<Ltree, Pg> for str {
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Pg>) -> serialize::Result {
        <str as ToSql<Text, Pg>>::to_sql(self, out)
    }
}

impl ToSql<Ltree, Pg> for String {
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Pg>) -> serialize::Result {
        <str as ToSql<Ltree, Pg>>::to_sql(self.as_str(), out)
    }
}

impl ToSql<Ltree, Pg> for LtreePath {
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Pg>) -> serialize::Result {
        <str as ToSql<Text, Pg>>::to_sql(self.0.as_str(), out)
    }
}

impl FromSql<Ltree, Pg> for String {
    fn from_sql(bytes: <Pg as Backend>::RawValue<'_>) -> deserialize::Result<Self> {
        <String as FromSql<Text, Pg>>::from_sql(bytes)
    }
}

impl FromSql<Ltree, Pg> for LtreePath {
    fn from_sql(bytes: <Pg as Backend>::RawValue<'_>) -> deserialize::Result<Self> {
        let value = <String as FromSql<Text, Pg>>::from_sql(bytes)?;
        Ok(LtreePath(value))
    }
}
