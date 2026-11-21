//! `GET /v1/symbol/{symbol_id}/references` — the fetch-more request (F2).
//!
//! A node the `/query` response listed in `edges_truncated` had its implicit
//! out-edges capped (assembly capping).  This endpoint pages the rest,
//! symbol-keyed: `kind=refs` (default) lists the references OUT of the
//! symbol in the engine's own `(from_instance, ref)` unit; `kind=has` lists
//! the instances it contains.  Keyset cursor: the response's `next` string
//! goes back verbatim as `?after=`; `null` means done.  Counts are CAPPED
//! ("`count_capped: true` = at least this many") — an exact out-edge total
//! for a hub node would be the unbounded scan the bounded-reads work
//! eliminated.

use actix_web::{get, web, HttpResponse, Responder};
use index::db_diesel::{EdgeOccurrenceRow, SymbolReferenceRow};
use index::offset_range::range_bounds_to_offsets;
use index::roles::{RefKind, Stated};
use index::symbols::SymbolType;
use serde::{Deserialize, Serialize};

use super::query::{narrow_roots, parse_project_list, QueryError};
use super::types::AsklData;

/// Page-size bounds: a missing `limit` serves one hover-screenful; the max
/// keeps a single response bounded no matter what the client asks for.
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 500;
/// Counts are exact up to this bound, "`COUNT_CAP`+" beyond it.
const COUNT_CAP: usize = 1000;

#[derive(Debug, Deserialize)]
pub struct ReferencesOpts {
    /// `refs` (default) or `has`.
    kind: Option<String>,
    /// Opaque cursor from the previous page's `next`.
    after: Option<String>,
    /// Rows per page (default 50, max 500).
    limit: Option<usize>,
    /// Same narrowing as `/query`'s `projects=`.
    projects: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ReferenceRowJson {
    /// Target symbol of the reference (`Node.id` encoding).
    pub to_symbol: String,
    pub to_symbol_name: String,
    /// Lowest-id instance of the target — where "open target" should land.
    pub to_instance_id: String,
    /// The instance of the queried symbol the reference was made from.
    pub from_instance_id: String,
    /// Call-site location.
    pub object_id: String,
    pub path: String,
    pub project_id: Option<String>,
    pub start_offset: i32,
    pub end_offset: i32,
    /// What this reference does to its target (see `Edge::kinds`); absent
    /// when nothing is stated.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<RefKind>,
}

/// One reference as the listing returns it.  `None` for a self-reference,
/// which has no other instance to open and which the engine would not draw
/// either, and for a row whose range has no bounds.
fn reference_json(row: SymbolReferenceRow) -> Option<ReferenceRowJson> {
    let to_instance_id = row.to_instance_id?;
    let (start_offset, end_offset) = range_bounds_to_offsets(&row.from_offset_range)?;
    let kinds = Stated::of(row.roles, row.relation).kinds(SymbolType::from(row.to_symbol_type));
    Some(ReferenceRowJson {
        to_symbol: row.to_symbol.to_string(),
        to_symbol_name: row.to_symbol_name,
        to_instance_id: to_instance_id.to_string(),
        from_instance_id: row.from_instance_id.to_string(),
        object_id: row.from_object.to_string(),
        path: row.path,
        project_id: row.project_id,
        start_offset,
        end_offset,
        kinds,
    })
}

#[derive(Debug, Serialize)]
pub struct ContainedRowJson {
    pub child_symbol: String,
    pub child_symbol_name: String,
    pub child_instance_id: String,
    pub parent_instance_id: String,
    pub object_id: String,
    pub path: String,
    pub project_id: Option<String>,
    pub start_offset: i32,
    pub end_offset: i32,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ReferenceRows {
    Refs(Vec<ReferenceRowJson>),
    Has(Vec<ContainedRowJson>),
}

#[derive(Debug, Serialize)]
pub struct ReferencesResponse {
    pub kind: String,
    pub rows: ReferenceRows,
    /// Cursor for the next page; `null` when this page is the last.
    pub next: Option<String>,
    /// Count of rows of this kind, exact unless `count_capped`.
    pub count: usize,
    /// True when `count` hit its bound: read it as "`count`+".
    pub count_capped: bool,
}

/// Format the keyset cursor as it travels to the client.
fn format_cursor(from_instance_id: i64, row_id: i64) -> String {
    format!("{from_instance_id}:{row_id}")
}

/// Parse a client cursor. `None` input = first page; malformed = `Err`.
fn parse_cursor(after: Option<&str>) -> Result<Option<(i64, i64)>, String> {
    let Some(raw) = after else { return Ok(None) };
    let mut parts = raw.splitn(2, ':');
    let (Some(f), Some(r)) = (parts.next(), parts.next()) else {
        return Err(format!("malformed cursor '{raw}'; expected '<from>:<id>'"));
    };
    match (f.parse::<i64>(), r.parse::<i64>()) {
        (Ok(f), Ok(r)) => Ok(Some((f, r))),
        _ => Err(format!("malformed cursor '{raw}'; expected two integers")),
    }
}

/// Format an instance-page cursor: the display keyset `(object, offset, id)`.
fn format_instance_cursor(object_id: i32, start_offset: i32, instance_id: i64) -> String {
    format!("{object_id}:{start_offset}:{instance_id}")
}

/// Parse an instance-page cursor. `None` = first page; malformed = `Err`.
fn parse_instance_cursor(after: Option<&str>) -> Result<index::db_diesel::DisplayCursor, String> {
    let Some(raw) = after else { return Ok(None) };
    let mut parts = raw.splitn(3, ':');
    let (Some(o), Some(off), Some(i)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(format!(
            "malformed cursor '{raw}'; expected '<object>:<offset>:<id>'"
        ));
    };
    match (o.parse::<i32>(), off.parse::<i32>(), i.parse::<i64>()) {
        (Ok(o), Ok(off), Ok(i)) => Ok(Some((o, off, i))),
        _ => Err(format!("malformed cursor '{raw}'; expected three integers")),
    }
}

/// Clamp a requested page size into [1, MAX_LIMIT].
fn clamp_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

#[get("/v1/symbol/{symbol_id}/references")]
pub async fn symbol_references(
    data: web::Data<AsklData>,
    path: web::Path<i64>,
    opts: web::Query<ReferencesOpts>,
) -> impl Responder {
    let symbol_id = path.into_inner();
    let kind = opts.kind.as_deref().unwrap_or("refs");
    if kind != "refs" && kind != "has" {
        return HttpResponse::BadRequest()
            .body(format!("unknown kind '{kind}'; expected 'refs' or 'has'"));
    }
    let after = match parse_cursor(opts.after.as_deref()) {
        Ok(after) => after,
        Err(msg) => return HttpResponse::BadRequest().body(msg),
    };
    let limit = clamp_limit(opts.limit);

    // Roots-only visibility, narrowed like /query: no ExecutionContext needed.
    let index = &data.cfg.index;
    let roots = match index.load_root_layers().await {
        Ok(roots) => roots,
        Err(err) => {
            return HttpResponse::InternalServerError()
                .body(format!("Failed to resolve root layers: {err}"))
        }
    };
    let projects = parse_project_list(opts.projects.as_deref());
    let roots = match narrow_roots(roots, &projects) {
        Ok(roots) => roots,
        Err(QueryError::Request(msg)) => return HttpResponse::BadRequest().body(msg),
        Err(_) => return HttpResponse::InternalServerError().body("project narrowing failed"),
    };
    let eph = index::db_diesel::EphContext::rooted(roots);

    let result = if kind == "refs" {
        let page = index
            .find_symbol_references(symbol_id, after, limit, &eph)
            .await;
        let count = index
            .count_symbol_references(symbol_id, COUNT_CAP, &eph)
            .await;
        match (page, count) {
            (Ok((rows, has_more)), Ok((count, count_capped))) => {
                let next = has_more
                    .then(|| rows.last())
                    .flatten()
                    .map(|r| format_cursor(r.from_instance_id, r.ref_id));
                // A dropped row's scan position was already consumed above,
                // so `next` stays honest.
                let rows = rows.into_iter().filter_map(reference_json).collect();
                Ok(ReferencesResponse {
                    kind: kind.to_string(),
                    rows: ReferenceRows::Refs(rows),
                    next,
                    count,
                    count_capped,
                })
            }
            (Err(err), _) | (_, Err(err)) => Err(err),
        }
    } else {
        let page = index
            .find_symbol_contained(symbol_id, after, limit, &eph)
            .await;
        let count = index
            .count_symbol_contained(symbol_id, COUNT_CAP, &eph)
            .await;
        match (page, count) {
            (Ok((rows, has_more)), Ok((count, count_capped))) => {
                let next = has_more
                    .then(|| rows.last())
                    .flatten()
                    .map(|r| format_cursor(r.parent_instance_id, r.child_instance_id));
                let rows = rows
                    .into_iter()
                    .filter_map(|r| {
                        let (start_offset, end_offset) = range_bounds_to_offsets(&r.offset_range)?;
                        Some(ContainedRowJson {
                            child_symbol: r.child_symbol.to_string(),
                            child_symbol_name: r.child_symbol_name,
                            child_instance_id: r.child_instance_id.to_string(),
                            parent_instance_id: r.parent_instance_id.to_string(),
                            object_id: r.object_id.to_string(),
                            path: r.path,
                            project_id: r.project_id,
                            start_offset,
                            end_offset,
                        })
                    })
                    .collect();
                Ok(ReferencesResponse {
                    kind: kind.to_string(),
                    rows: ReferenceRows::Has(rows),
                    next,
                    count,
                    count_capped,
                })
            }
            (Err(err), _) | (_, Err(err)) => Err(err),
        }
    };

    match result {
        Ok(resp) => HttpResponse::Ok().json(resp),
        Err(err) => {
            HttpResponse::InternalServerError().body(format!("references query failed: {err}"))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct InstancesOpts {
    /// Opaque cursor from the previous page's `next` (an `object:offset:id`
    /// keyset in display order), passed back verbatim.
    after: Option<String>,
    /// Rows per page (default 50, max 500).
    limit: Option<usize>,
    /// Containment context: list only the instances contained by this PARENT
    /// symbol. The same symbol under several parents renders as several
    /// nodes with disjoint instance sets; each pages only its own partition.
    /// Absent = every instance of the symbol.
    parent: Option<i64>,
    /// Same narrowing as `/query`'s `projects=`.
    projects: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InstanceRowJson {
    pub id: String,
    pub instance_type: String,
    pub object_id: String,
    pub path: String,
    pub project_id: Option<String>,
    pub start_offset: i32,
    pub end_offset: i32,
}

#[derive(Debug, Serialize)]
pub struct InstancesResponse {
    pub rows: Vec<InstanceRowJson>,
    /// Opaque `object:offset:id` cursor for the next page; `null` when done.
    pub next: Option<String>,
    /// EXACT instance count (per-symbol counts are bounded by the symbol's
    /// own size, unlike edge counts).
    pub total: usize,
}

/// `GET /v1/symbol/{symbol_id}/instances` — pages a node whose
/// `symbol_instances` the `/query` response truncated
/// (`Node.instances_truncated`).  Keyset by instance id; display ordering is
/// the client's concern (it re-sorts by path/offset anyway).
#[get("/v1/symbol/{symbol_id}/instances")]
pub async fn symbol_instances(
    data: web::Data<AsklData>,
    path: web::Path<i64>,
    opts: web::Query<InstancesOpts>,
) -> impl Responder {
    let symbol_id = path.into_inner();
    let limit = clamp_limit(opts.limit);

    let index = &data.cfg.index;
    let roots = match index.load_root_layers().await {
        Ok(roots) => roots,
        Err(err) => {
            return HttpResponse::InternalServerError()
                .body(format!("Failed to resolve root layers: {err}"))
        }
    };
    let projects = parse_project_list(opts.projects.as_deref());
    let roots = match narrow_roots(roots, &projects) {
        Ok(roots) => roots,
        Err(QueryError::Request(msg)) => return HttpResponse::BadRequest().body(msg),
        Err(_) => return HttpResponse::InternalServerError().body("project narrowing failed"),
    };
    let eph = index::db_diesel::EphContext::rooted(roots);

    let after = match parse_instance_cursor(opts.after.as_deref()) {
        Ok(cursor) => cursor,
        Err(msg) => return HttpResponse::BadRequest().body(msg),
    };

    let page = match opts.parent {
        Some(parent) => {
            index
                .list_symbol_instances_under(symbol_id, parent, after, limit, &eph)
                .await
        }
        None => {
            index
                .list_symbol_instances(symbol_id, after, limit, &eph)
                .await
        }
    };
    let total = index.count_symbol_instances(symbol_id, &eph).await;
    match (page, total) {
        (Ok((rows, has_more)), Ok(total)) => {
            // `next` is the last row's display keyset. Compute from the raw
            // row (which carries object_id + offset_range) before mapping.
            let next = if has_more {
                rows.last().and_then(|r| {
                    let (start_offset, _) = range_bounds_to_offsets(&r.offset_range)?;
                    Some(format_instance_cursor(
                        r.object_id,
                        start_offset,
                        r.instance_id,
                    ))
                })
            } else {
                None
            };
            let rows = rows
                .into_iter()
                .filter_map(|r| {
                    let (start_offset, end_offset) = range_bounds_to_offsets(&r.offset_range)?;
                    Some(InstanceRowJson {
                        id: r.instance_id.to_string(),
                        instance_type: format!(
                            "{:?}",
                            index::symbols::InstanceType::from(r.instance_type as i64)
                        )
                        .to_lowercase(),
                        object_id: r.object_id.to_string(),
                        path: r.path,
                        project_id: r.project_id,
                        start_offset,
                        end_offset,
                    })
                })
                .collect();
            HttpResponse::Ok().json(InstancesResponse { rows, next, total })
        }
        (Err(err), _) | (_, Err(err)) => {
            HttpResponse::InternalServerError().body(format!("instances query failed: {err}"))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct EdgeOccurrencesOpts {
    /// Opaque cursor from the previous page's `next`.
    after: Option<String>,
    /// Rows per page (default 50, max 500).
    limit: Option<usize>,
    /// Same narrowing as `/query`'s `projects=`.
    projects: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EdgeOccurrenceRowJson {
    pub ref_id: String,
    pub from_instance_id: String,
    pub object_id: String,
    pub path: String,
    pub project_id: Option<String>,
    pub start_offset: i32,
    pub end_offset: i32,
    /// What THIS occurrence does to the target (see `Edge::kinds`, which is
    /// the union over the pair's occurrences); absent when nothing is stated.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<RefKind>,
}

/// One call site as the endpoint returns it; `None` for a row whose range
/// has no bounds.
fn edge_occurrence_json(row: EdgeOccurrenceRow) -> Option<EdgeOccurrenceRowJson> {
    let (start_offset, end_offset) = range_bounds_to_offsets(&row.from_offset_range)?;
    let kinds = Stated::of(row.roles, row.relation).kinds(SymbolType::from(row.to_symbol_type));
    Some(EdgeOccurrenceRowJson {
        ref_id: row.ref_id.to_string(),
        from_instance_id: row.from_instance_id.to_string(),
        object_id: row.from_object.to_string(),
        path: row.path,
        project_id: row.project_id,
        start_offset,
        end_offset,
        kinds,
    })
}

#[derive(Debug, Serialize)]
pub struct EdgeOccurrencesResponse {
    pub rows: Vec<EdgeOccurrenceRowJson>,
    /// Opaque `object:offset:id` cursor for the next page; `null` when done.
    pub next: Option<String>,
}

/// `GET /v1/edge/{from_symbol}/{to_symbol}/occurrences` — the call sites of
/// ONE edge. An edge is a (from, to) symbol pair, so its occurrences are their
/// own listing; `/query` ships only the pair plus a count, and a client pages
/// the call sites here when it opens the edge.
#[get("/v1/edge/{from_symbol}/{to_symbol}/occurrences")]
pub async fn edge_occurrences(
    data: web::Data<AsklData>,
    path: web::Path<(i64, i64)>,
    opts: web::Query<EdgeOccurrencesOpts>,
) -> impl Responder {
    let (from_symbol, to_symbol) = path.into_inner();
    let limit = clamp_limit(opts.limit);

    let index = &data.cfg.index;
    let roots = match index.load_root_layers().await {
        Ok(roots) => roots,
        Err(err) => {
            return HttpResponse::InternalServerError()
                .body(format!("Failed to resolve root layers: {err}"))
        }
    };
    let projects = parse_project_list(opts.projects.as_deref());
    let roots = match narrow_roots(roots, &projects) {
        Ok(roots) => roots,
        Err(QueryError::Request(msg)) => return HttpResponse::BadRequest().body(msg),
        Err(_) => return HttpResponse::InternalServerError().body("project narrowing failed"),
    };
    let eph = index::db_diesel::EphContext::rooted(roots);

    let after = match parse_instance_cursor(opts.after.as_deref()) {
        Ok(cursor) => cursor,
        Err(msg) => return HttpResponse::BadRequest().body(msg),
    };

    match index
        .find_edge_occurrences(from_symbol, to_symbol, after, limit, &eph)
        .await
    {
        Ok((rows, has_more)) => {
            let next = if has_more {
                rows.last()
                    .map(|r| format_instance_cursor(r.from_object, r.start_offset(), r.ref_id))
            } else {
                None
            };
            let rows = rows.into_iter().filter_map(edge_occurrence_json).collect();
            HttpResponse::Ok().json(EdgeOccurrencesResponse { rows, next })
        }
        Err(err) => {
            HttpResponse::InternalServerError().body(format!("edge occurrences failed: {err}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn occurrence(roles: i16, relation: i16, to_symbol_type: i32) -> EdgeOccurrenceRow {
        EdgeOccurrenceRow {
            ref_id: 7,
            from_instance_id: 10,
            from_object: 1,
            path: "/a.c".to_string(),
            project_id: Some("1".to_string()),
            from_offset_range: (
                std::ops::Bound::Included(130),
                std::ops::Bound::Excluded(134),
            ),
            roles,
            relation,
            to_symbol_type,
        }
    }

    #[test]
    fn an_occurrence_says_what_it_does_to_the_target() {
        // `hook++` on a data symbol: written, and its value read.
        let row = edge_occurrence_json(occurrence(6, 0, 6)).unwrap();
        assert_eq!(
            serde_json::to_value(&row).unwrap()["kinds"],
            serde_json::json!(["writes", "reads"])
        );
        // The same roles on a function: its value is taken, not read.
        let row = edge_occurrence_json(occurrence(4, 0, 1)).unwrap();
        assert_eq!(
            serde_json::to_value(&row).unwrap()["kinds"],
            serde_json::json!(["takes"])
        );
        // A relation is not something the token says, and prints as itself.
        let row = edge_occurrence_json(occurrence(0, 2, 1)).unwrap();
        assert_eq!(
            serde_json::to_value(&row).unwrap()["kinds"],
            serde_json::json!(["holds"])
        );
        // Nothing stated: the row is what it was, with no `kinds` key.
        let row = edge_occurrence_json(occurrence(0, 0, 1)).unwrap();
        assert_eq!(
            serde_json::to_string(&row).unwrap(),
            r#"{"ref_id":"7","from_instance_id":"10","object_id":"1","path":"/a.c","project_id":"1","start_offset":130,"end_offset":134}"#
        );
    }

    fn listed(roles: i16, relation: i16, to_symbol_type: i32) -> SymbolReferenceRow {
        SymbolReferenceRow {
            ref_id: 7,
            from_instance_id: 10,
            to_symbol: 2,
            to_symbol_name: "hook".to_string(),
            to_instance_id: Some(20),
            from_object: 1,
            path: "/a.c".to_string(),
            project_id: Some("1".to_string()),
            from_offset_range: (
                std::ops::Bound::Included(130),
                std::ops::Bound::Excluded(134),
            ),
            roles,
            relation,
            to_symbol_type,
        }
    }

    #[test]
    fn a_listed_reference_says_what_it_does_to_the_target() {
        let row = reference_json(listed(6, 0, 6)).unwrap();
        assert_eq!(
            serde_json::to_value(&row).unwrap()["kinds"],
            serde_json::json!(["writes", "reads"])
        );
        let row = reference_json(listed(1, 0, 1)).unwrap();
        assert_eq!(
            serde_json::to_value(&row).unwrap()["kinds"],
            serde_json::json!(["calls"])
        );
        let row = reference_json(listed(0, 1, 6)).unwrap();
        assert_eq!(
            serde_json::to_value(&row).unwrap()["kinds"],
            serde_json::json!(["declares"])
        );
        // Nothing stated: the row is what it was, with no `kinds` key.
        let row = reference_json(listed(0, 0, 1)).unwrap();
        assert_eq!(
            serde_json::to_string(&row).unwrap(),
            r#"{"to_symbol":"2","to_symbol_name":"hook","to_instance_id":"20","from_instance_id":"10","object_id":"1","path":"/a.c","project_id":"1","start_offset":130,"end_offset":134}"#
        );
        // A self-reference has no other instance to open: not listed.
        let mut own = listed(1, 0, 1);
        own.to_instance_id = None;
        assert!(reference_json(own).is_none());
    }

    #[test]
    fn cursor_round_trips_and_rejects_garbage() {
        assert_eq!(parse_cursor(None).unwrap(), None);
        let s = format_cursor(4001, 123);
        assert_eq!(parse_cursor(Some(&s)).unwrap(), Some((4001, 123)));
        assert!(parse_cursor(Some("nope")).is_err());
        assert!(parse_cursor(Some("1:two")).is_err());
    }

    #[test]
    fn limit_is_clamped_to_sane_bounds() {
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(10_000)), MAX_LIMIT);
        assert_eq!(clamp_limit(Some(7)), 7);
    }
}
