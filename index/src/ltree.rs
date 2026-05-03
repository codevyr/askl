use std::io::Write;

use diesel::backend::Backend;
use diesel::deserialize::{self, FromSql};
use diesel::pg::Pg;
use diesel::serialize::{self, IsNull, Output, ToSql};
use diesel::sql_types::SqlType;

/// SQL type marker for the PostgreSQL `ltree` type.
#[derive(SqlType)]
#[diesel(postgres_type(name = "ltree"))]
pub struct Ltree;

impl ToSql<Ltree, Pg> for str {
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Pg>) -> serialize::Result {
        // PostgreSQL ltree binary format: 1-byte version (must be 1) + label path bytes.
        // Delegating to ToSql<Text> omits the version byte, causing "unsupported ltree
        // version number N" at the server when the first character isn't ASCII 1.
        out.write_all(&[1u8])?;
        out.write_all(self.as_bytes())?;
        Ok(IsNull::No)
    }
}

impl ToSql<Ltree, Pg> for String {
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Pg>) -> serialize::Result {
        <str as ToSql<Ltree, Pg>>::to_sql(self.as_str(), out)
    }
}

impl FromSql<Ltree, Pg> for String {
    fn from_sql(bytes: <Pg as Backend>::RawValue<'_>) -> deserialize::Result<Self> {
        // Strip the leading version byte before handing off to text deserialization.
        let bytes = bytes.as_bytes();
        let payload = if bytes.first() == Some(&1) { &bytes[1..] } else { bytes };
        std::str::from_utf8(payload)
            .map(|s| s.to_owned())
            .map_err(|e| e.into())
    }
}

/// Rust-side value type for `ltree` columns in `Insertable` structs.
///
/// `String` cannot implement `AsExpression<Ltree>` due to Diesel's orphan rules,
/// so this newtype is used with `#[diesel(serialize_as = "LtreeValue")]` on
/// struct fields that need to insert into an `ltree` column.
#[derive(Debug, Clone, diesel::expression::AsExpression)]
#[diesel(sql_type = Ltree)]
pub struct LtreeValue(pub String);

impl ToSql<Ltree, Pg> for LtreeValue {
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Pg>) -> serialize::Result {
        <str as ToSql<Ltree, Pg>>::to_sql(&self.0, out)
    }
}

impl From<String> for LtreeValue {
    fn from(s: String) -> Self {
        LtreeValue(s)
    }
}
