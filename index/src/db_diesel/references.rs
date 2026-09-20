//! Paginated per-symbol reference/containment reads for the fetch-more API
//! (F2): "the instances referenced by symbol A", keyset-paginated.
//!
//! Semantics mirror the engine's edge discovery (`find_edges_between`): the
//! same containment rule ties a reference to its enclosing source instance
//! (the parent's DFS interval encloses the ref's `dfs_enter`), the row unit is the
//! engine's own dedup key `(from_instance, ref)`, and the target instance is
//! the lowest-id instance of the referenced symbol other than the source —
//! resolved per PAGE row through a LATERAL, not per scanned row.
//!
//! The page shape is built for streaming: the source instances are walked in
//! id order, each one's references in ref-id order (a LATERAL preserves the
//! outer order, so the planner runs an incremental sort and the LIMIT stops
//! after the first source instances).  Measured on the live index: 3 ms for
//! the worst hub node (931 instances, ~1 M refs) vs 2.9 s for the naive
//! DISTINCT-ON form, and sub-millisecond for ordinary nodes.
//!
//! Assembly capping territory: these reads serve pagination of a settled
//! node's neighbourhood.  Nothing here touches `Selection` or warnings.
//!
//! Visibility: one combined query over `eph.visible_ids()`.  The endpoint
//! context is roots-only, so no eph-branch disjointness guard is needed; a
//! caller with materialised eph chains would see chain rows without the
//! partition-cache treatment (acceptable: results are not SQL-cached).

use anyhow::Result;
use diesel::sql_types::{Array, BigInt, Int4range, Integer, Nullable, Text};
use diesel_async::RunQueryDsl;

use super::index_impl::Index;
use super::selection::EphContext;

/// One reference out of the queried symbol: the engine's `(from_instance,
/// ref)` unit plus what a client needs to render and open the call site.
#[derive(diesel::QueryableByName, Debug, Clone)]
pub struct SymbolReferenceRow {
    #[diesel(sql_type = BigInt)]
    pub ref_id: i64,
    #[diesel(sql_type = BigInt)]
    pub from_instance_id: i64,
    #[diesel(sql_type = BigInt)]
    pub to_symbol: i64,
    #[diesel(sql_type = Text)]
    pub to_symbol_name: String,
    /// Lowest-id instance of the target other than the source — where "open
    /// target" should land.  `None` when the referenced symbol has no other
    /// instance (a self-reference): the engine drops such edges, and the
    /// endpoint filters these rows out of the response — but they still
    /// occupy scan positions, so pagination sees them (see `find_symbol_references`).
    #[diesel(sql_type = Nullable<BigInt>)]
    pub to_instance_id: Option<i64>,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub from_object: i32,
    #[diesel(sql_type = Text)]
    pub path: String,
    #[diesel(sql_type = Nullable<Text>)]
    pub project_id: Option<String>,
    #[diesel(sql_type = Int4range)]
    pub from_offset_range: (std::ops::Bound<i32>, std::ops::Bound<i32>),
}

/// One instance contained by the queried symbol (the has-kind page row).
#[derive(diesel::QueryableByName, Debug, Clone)]
pub struct ContainedInstanceRow {
    #[diesel(sql_type = BigInt)]
    pub parent_instance_id: i64,
    #[diesel(sql_type = BigInt)]
    pub child_instance_id: i64,
    #[diesel(sql_type = BigInt)]
    pub child_symbol: i64,
    #[diesel(sql_type = Text)]
    pub child_symbol_name: String,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub object_id: i32,
    #[diesel(sql_type = Text)]
    pub path: String,
    #[diesel(sql_type = Nullable<Text>)]
    pub project_id: Option<String>,
    #[diesel(sql_type = Int4range)]
    pub offset_range: (std::ops::Bound<i32>, std::ops::Bound<i32>),
}

/// One instance of the queried symbol (the instance-pagination page row).
#[derive(diesel::QueryableByName, Debug, Clone)]
pub struct SymbolInstancePageRow {
    #[diesel(sql_type = BigInt)]
    pub instance_id: i64,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub instance_type: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub object_id: i32,
    #[diesel(sql_type = Text)]
    pub path: String,
    #[diesel(sql_type = Nullable<Text>)]
    pub project_id: Option<String>,
    #[diesel(sql_type = Int4range)]
    pub offset_range: (std::ops::Bound<i32>, std::ops::Bound<i32>),
}

impl SymbolInstancePageRow {
    /// The instance's start offset (lower bound of its range) — the middle
    /// component of the display keyset cursor.
    pub fn start_offset(&self) -> i32 {
        match self.offset_range.0 {
            std::ops::Bound::Included(v) => v,
            std::ops::Bound::Excluded(v) => v + 1,
            std::ops::Bound::Unbounded => i32::MIN,
        }
    }
}

/// One call site of an edge (a reference from one symbol to another).
#[derive(diesel::QueryableByName, Debug, Clone)]
pub struct EdgeOccurrenceRow {
    #[diesel(sql_type = BigInt)]
    pub ref_id: i64,
    #[diesel(sql_type = BigInt)]
    pub from_instance_id: i64,
    #[diesel(sql_type = Integer)]
    pub from_object: i32,
    #[diesel(sql_type = Text)]
    pub path: String,
    #[diesel(sql_type = Nullable<Text>)]
    pub project_id: Option<String>,
    #[diesel(sql_type = Int4range)]
    pub from_offset_range: (std::ops::Bound<i32>, std::ops::Bound<i32>),
}

impl EdgeOccurrenceRow {
    /// Start offset of the call site — middle component of the display cursor.
    pub fn start_offset(&self) -> i32 {
        match self.from_offset_range.0 {
            std::ops::Bound::Included(v) => v,
            std::ops::Bound::Excluded(v) => v + 1,
            std::ops::Bound::Unbounded => i32::MIN,
        }
    }
}

#[derive(diesel::QueryableByName, Debug)]
struct CountRow {
    #[diesel(sql_type = BigInt)]
    n: i64,
}

/// Keyset cursor: the engine's row key `(from_instance_id, ref/child id)`.
/// `None` starts from the beginning.
pub type ReferenceCursor = Option<(i64, i64)>;

/// Keyset cursor in DISPLAY order — `(object_id, start_offset, row_id)`.
/// Used by instance pages and by edge-occurrence pages: cheap ints, local to
/// the listing being paged, continuous, so pages append contiguously to what
/// the UI shows. `None` = first page.
pub type DisplayCursor = Option<(i32, i32, i64)>;

fn cursor_parts(after: ReferenceCursor) -> (i64, i64) {
    after.unwrap_or((i64::MIN, i64::MIN))
}

impl Index {
    /// One page of the references OUT of `symbol_id`, ordered by
    /// `(from_instance_id, ref_id)` strictly after `after`.  Returns up to
    /// `limit` rows and whether more remain (the `limit + 1` sentinel).
    ///
    /// The target lookup is a LEFT join ON PURPOSE: a self-reference (the
    /// referenced symbol's only instance IS the source) yields
    /// `to_instance_id = None`.  Making that join lossy instead broke
    /// pagination in production — `has_more` was counted after the drop, so a
    /// page with one self-ref among `limit + 1` fetched rows reported
    /// `has_more = false` with 1000+ rows remaining.  Rows keep their scan
    /// position here; the caller filters `None` targets out of the RESPONSE
    /// while taking the cursor from the last RAW row.
    pub async fn find_symbol_references(
        &self,
        symbol_id: i64,
        after: ReferenceCursor,
        limit: usize,
        eph: &EphContext,
    ) -> Result<(Vec<SymbolReferenceRow>, bool)> {
        let (af, ar) = cursor_parts(after);
        let vis = eph.visible_ids();
        let fetch = limit as i64 + 1;
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let mut rows: Vec<SymbolReferenceRow> = diesel::sql_query(
            "SELECT page.ref_id, page.from_instance_id, page.to_symbol, \
                    ts.name AS to_symbol_name, ti.id AS to_instance_id, \
                    page.from_object, o.filesystem_path AS path, \
                    p.project_name AS project_id, page.from_offset_range \
             FROM ( \
               SELECT sr.id AS ref_id, sr.to_symbol, sr.from_object, \
                      sr.from_offset_range, from_inst.id AS from_instance_id \
               FROM ( \
                 SELECT si.id, si.object_id, si.offset_range, si.dfs_enter, si.dfs_exit \
                 FROM index.symbol_instances si \
                 WHERE si.symbol = $1 AND si.layer = ANY($2) AND si.id >= $3 \
                 ORDER BY si.id \
               ) from_inst \
               CROSS JOIN LATERAL ( \
                 SELECT sr.id, sr.to_symbol, sr.from_object, sr.from_offset_range \
                 FROM index.symbol_refs sr \
                 WHERE sr.from_object = from_inst.object_id \
                   AND from_inst.dfs_enter < sr.dfs_enter \
                   AND sr.dfs_enter < from_inst.dfs_exit \
                   AND sr.layer = ANY($2) \
                   AND (from_inst.id > $3 OR sr.id > $4) \
                 ORDER BY sr.id \
               ) sr \
               ORDER BY from_inst.id, sr.id \
               LIMIT $5 \
             ) page \
             LEFT JOIN LATERAL ( \
               SELECT ti.id FROM index.symbol_instances ti \
               WHERE ti.symbol = page.to_symbol AND ti.layer = ANY($2) \
                 AND ti.id != page.from_instance_id \
               ORDER BY ti.id LIMIT 1 \
             ) ti ON true \
             JOIN index.symbols ts ON ts.id = page.to_symbol AND ts.layer = ANY($2) \
             JOIN index.objects o ON o.id = page.from_object \
             LEFT JOIN index.projects p ON p.id = o.project_id \
             ORDER BY page.from_instance_id, page.ref_id",
        )
        .bind::<BigInt, _>(symbol_id)
        .bind::<Array<BigInt>, _>(&vis)
        .bind::<BigInt, _>(af)
        .bind::<BigInt, _>(ar)
        .bind::<BigInt, _>(fetch)
        .get_results(&mut *connection)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to page symbol references: {}", e))?;

        let has_more = rows.len() > limit;
        rows.truncate(limit);
        Ok((rows, has_more))
    }

    /// One page of the instances CONTAINED by `symbol_id` (the has-kind),
    /// ordered by `(parent_instance_id, child_instance_id)` strictly after
    /// `after`.  Same streaming shape and sentinel as
    /// [`Self::find_symbol_references`]; the containment rule (same object,
    /// covering range, container type level >= contained's) mirrors the
    /// engine's has-children read.
    pub async fn find_symbol_contained(
        &self,
        symbol_id: i64,
        after: ReferenceCursor,
        limit: usize,
        eph: &EphContext,
    ) -> Result<(Vec<ContainedInstanceRow>, bool)> {
        let (af, ac) = cursor_parts(after);
        let vis = eph.visible_ids();
        let fetch = limit as i64 + 1;
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let mut rows: Vec<ContainedInstanceRow> = diesel::sql_query(
            "SELECT page.parent_instance_id, page.child_instance_id, \
                    page.child_symbol, cs.name AS child_symbol_name, \
                    page.object_id, o.filesystem_path AS path, \
                    p.project_name AS project_id, page.offset_range \
             FROM ( \
               SELECT from_inst.id AS parent_instance_id, ci.id AS child_instance_id, \
                      ci.symbol AS child_symbol, ci.object_id, ci.offset_range \
               FROM ( \
                 SELECT si.id, si.object_id, si.offset_range, si.dfs_enter, si.dfs_exit \
                 FROM index.symbol_instances si \
                 WHERE si.symbol = $1 AND si.layer = ANY($2) AND si.id >= $3 \
                 ORDER BY si.id \
               ) from_inst \
               CROSS JOIN LATERAL ( \
                 SELECT ci.id, ci.symbol, ci.object_id, ci.offset_range, ci.dfs_enter \
                 FROM index.symbol_instances ci \
                 JOIN index.symbols cs ON ci.symbol = cs.id \
                 WHERE ci.object_id = from_inst.object_id \
                   AND from_inst.dfs_enter < ci.dfs_enter \
                   AND ci.dfs_enter < from_inst.dfs_exit \
                   AND ci.layer = ANY($2) AND cs.layer = ANY($2) \
                   AND (from_inst.id > $3 OR ci.id > $4) \
                 ORDER BY ci.id \
               ) ci \
               ORDER BY from_inst.id, ci.id \
               LIMIT $5 \
             ) page \
             JOIN index.symbols cs ON cs.id = page.child_symbol AND cs.layer = ANY($2) \
             JOIN index.objects o ON o.id = page.object_id \
             LEFT JOIN index.projects p ON p.id = o.project_id \
             ORDER BY page.parent_instance_id, page.child_instance_id",
        )
        .bind::<BigInt, _>(symbol_id)
        .bind::<Array<BigInt>, _>(&vis)
        .bind::<BigInt, _>(af)
        .bind::<BigInt, _>(ac)
        .bind::<BigInt, _>(fetch)
        .get_results(&mut *connection)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to page contained instances: {}", e))?;

        let has_more = rows.len() > limit;
        rows.truncate(limit);
        Ok((rows, has_more))
    }

    /// Reference count for `symbol_id`, CAPPED at `cap`: returns
    /// `(n, capped)` where `capped` means "cap+ — at least this many".  An
    /// exact count over a hub node's references is the unbounded scan the
    /// bounded-reads work eliminated, so this counts through the same
    /// streaming shape and stops at `cap + 1` rows.
    pub async fn count_symbol_references(
        &self,
        symbol_id: i64,
        cap: usize,
        eph: &EphContext,
    ) -> Result<(usize, bool)> {
        let vis = eph.visible_ids();
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let row: CountRow = diesel::sql_query(
            "SELECT count(*) AS n FROM ( \
               SELECT 1 \
               FROM ( \
                 SELECT si.id, si.object_id, si.offset_range, si.dfs_enter, si.dfs_exit \
                 FROM index.symbol_instances si \
                 WHERE si.symbol = $1 AND si.layer = ANY($2) \
                 ORDER BY si.id \
               ) from_inst \
               CROSS JOIN LATERAL ( \
                 SELECT sr.id FROM index.symbol_refs sr \
                 WHERE sr.from_object = from_inst.object_id \
                   AND from_inst.dfs_enter < sr.dfs_enter \
                   AND sr.dfs_enter < from_inst.dfs_exit \
                   AND sr.layer = ANY($2) \
                 ORDER BY sr.id \
               ) sr \
               LIMIT $3 \
             ) c",
        )
        .bind::<BigInt, _>(symbol_id)
        .bind::<Array<BigInt>, _>(&vis)
        .bind::<BigInt, _>(cap as i64 + 1)
        .get_result(&mut *connection)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to count symbol references: {}", e))?;

        let n = row.n as usize;
        Ok((n.min(cap), n > cap))
    }

    /// One page of `symbol_id`'s instances, keyset-paginated on the DISPLAY
    /// order `(object_id, start_offset, id)` — cheap ints local to this
    /// symbol's instances — so pages are contiguous with what the UI shows
    /// (file then position) and "load more" is monotone. Replaces the earlier
    /// global-instance-id cursor, which scattered pages relative to the
    /// display. `after` = the previous page's last `(object_id, start_offset,
    /// id)`; `None` = first page. `limit + 1` sentinel decides `has_more`.
    pub async fn list_symbol_instances(
        &self,
        symbol_id: i64,
        after: DisplayCursor,
        limit: usize,
        eph: &EphContext,
    ) -> Result<(Vec<SymbolInstancePageRow>, bool)> {
        let (a_obj, a_off, a_id) = after.unwrap_or((i32::MIN, i32::MIN, i64::MIN));
        let vis = eph.visible_ids();
        let fetch = limit as i64 + 1;
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let mut rows: Vec<SymbolInstancePageRow> = diesel::sql_query(
            "SELECT si.id AS instance_id, si.instance_type, si.object_id, \
                    o.filesystem_path AS path, p.project_name AS project_id, \
                    si.offset_range \
             FROM index.symbol_instances si \
             JOIN index.objects o ON o.id = si.object_id \
             LEFT JOIN index.projects p ON p.id = o.project_id \
             WHERE si.symbol = $1 AND si.layer = ANY($2) \
               AND (si.object_id, lower(si.offset_range), si.id) > ($3, $4, $5) \
             ORDER BY si.object_id, lower(si.offset_range), si.id \
             LIMIT $6",
        )
        .bind::<BigInt, _>(symbol_id)
        .bind::<Array<BigInt>, _>(&vis)
        .bind::<Integer, _>(a_obj)
        .bind::<Integer, _>(a_off)
        .bind::<BigInt, _>(a_id)
        .bind::<BigInt, _>(fetch)
        .get_results(&mut *connection)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to page symbol instances: {}", e))?;

        let has_more = rows.len() > limit;
        rows.truncate(limit);
        Ok((rows, has_more))
    }

    /// One page of a symbol's instances CONTAINED BY `parent_symbol` — the
    /// listing behind a node that the server split into a containment
    /// context. The same symbol under several parents is several nodes with
    /// disjoint instance sets, so each pages only its own partition; the
    /// union of all parents plus the parentless case is the symbol's whole
    /// set, with no overlap. Same display-order keyset as the unscoped page.
    pub async fn list_symbol_instances_under(
        &self,
        symbol_id: i64,
        parent_symbol: i64,
        after: DisplayCursor,
        limit: usize,
        eph: &EphContext,
    ) -> Result<(Vec<SymbolInstancePageRow>, bool)> {
        let (a_obj, a_off, a_id) = after.unwrap_or((i32::MIN, i32::MIN, i64::MIN));
        let vis = eph.visible_ids();
        let fetch = limit as i64 + 1;
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let mut rows: Vec<SymbolInstancePageRow> = diesel::sql_query(
            "SELECT si.id AS instance_id, si.instance_type, si.object_id, \
                    o.filesystem_path AS path, p.project_name AS project_id, \
                    si.offset_range \
             FROM index.symbol_instances si \
             JOIN index.objects o ON o.id = si.object_id \
             LEFT JOIN index.projects p ON p.id = o.project_id \
             WHERE si.symbol = $1 AND si.layer = ANY($2) \
               AND (si.object_id, lower(si.offset_range), si.id) > ($4, $5, $6) \
               AND EXISTS ( \
                 SELECT 1 FROM index.symbol_instances pi \
                 WHERE pi.symbol = $3 AND pi.layer = ANY($2) \
                   AND pi.object_id = si.object_id \
                   AND pi.dfs_enter < si.dfs_enter \
                   AND si.dfs_enter < pi.dfs_exit \
               ) \
             ORDER BY si.object_id, lower(si.offset_range), si.id \
             LIMIT $7",
        )
        .bind::<BigInt, _>(symbol_id)
        .bind::<Array<BigInt>, _>(&vis)
        .bind::<BigInt, _>(parent_symbol)
        .bind::<Integer, _>(a_obj)
        .bind::<Integer, _>(a_off)
        .bind::<BigInt, _>(a_id)
        .bind::<BigInt, _>(fetch)
        .get_results(&mut *connection)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to page scoped instances: {}", e))?;

        let has_more = rows.len() > limit;
        rows.truncate(limit);
        Ok((rows, has_more))
    }

    /// One page of the call sites of ONE edge — the references from
    /// `from_symbol` to `to_symbol`. An edge IS a (from, to) pair, so this is
    /// its own listing rather than a filter over the whole references stream.
    /// Same containment rule as edge discovery (a reference belongs to the
    /// source instance whose range covers it). Paged in DISPLAY order
    /// `(from_object, from_offset, ref_id)` so pages append contiguously.
    pub async fn find_edge_occurrences(
        &self,
        from_symbol: i64,
        to_symbol: i64,
        after: DisplayCursor,
        limit: usize,
        eph: &EphContext,
    ) -> Result<(Vec<EdgeOccurrenceRow>, bool)> {
        let (a_obj, a_off, a_id) = after.unwrap_or((i32::MIN, i32::MIN, i64::MIN));
        let vis = eph.visible_ids();
        let fetch = limit as i64 + 1;
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let mut rows: Vec<EdgeOccurrenceRow> = diesel::sql_query(
            "SELECT sr.id AS ref_id, from_inst.id AS from_instance_id, \
                    sr.from_object, o.filesystem_path AS path, \
                    p.project_name AS project_id, sr.from_offset_range \
             FROM ( \
               SELECT si.id, si.object_id, si.offset_range, si.dfs_enter, si.dfs_exit \
               FROM index.symbol_instances si \
               WHERE si.symbol = $1 AND si.layer = ANY($2) \
             ) from_inst \
             CROSS JOIN LATERAL ( \
               SELECT sr.id, sr.from_object, sr.from_offset_range \
               FROM index.symbol_refs sr \
               WHERE sr.from_object = from_inst.object_id \
                 AND from_inst.dfs_enter < sr.dfs_enter \
                 AND sr.dfs_enter < from_inst.dfs_exit \
                 AND sr.layer = ANY($2) \
                 AND sr.to_symbol = $3 \
             ) sr \
             JOIN index.objects o ON o.id = sr.from_object \
             LEFT JOIN index.projects p ON p.id = o.project_id \
             WHERE (sr.from_object, lower(sr.from_offset_range), sr.id) > ($4, $5, $6) \
             ORDER BY sr.from_object, lower(sr.from_offset_range), sr.id \
             LIMIT $7",
        )
        .bind::<BigInt, _>(from_symbol)
        .bind::<Array<BigInt>, _>(&vis)
        .bind::<BigInt, _>(to_symbol)
        .bind::<Integer, _>(a_obj)
        .bind::<Integer, _>(a_off)
        .bind::<BigInt, _>(a_id)
        .bind::<BigInt, _>(fetch)
        .get_results(&mut *connection)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to page edge occurrences: {}", e))?;

        let has_more = rows.len() > limit;
        rows.truncate(limit);
        Ok((rows, has_more))
    }

    /// EXACT instance count for `symbol_id` — one index-driven count over a
    /// single symbol's instances, bounded by that symbol's own size (unlike
    /// edge counts, which must stay capped).
    pub async fn count_symbol_instances(&self, symbol_id: i64, eph: &EphContext) -> Result<usize> {
        let vis = eph.visible_ids();
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let row: CountRow = diesel::sql_query(
            "SELECT count(*) AS n FROM index.symbol_instances si \
             WHERE si.symbol = $1 AND si.layer = ANY($2)",
        )
        .bind::<BigInt, _>(symbol_id)
        .bind::<Array<BigInt>, _>(&vis)
        .get_result(&mut *connection)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to count symbol instances: {}", e))?;

        Ok(row.n as usize)
    }

    /// Contained-instance count for `symbol_id`, CAPPED at `cap` — the
    /// has-kind mirror of [`Self::count_symbol_references`], same shape and
    /// same "cap+" contract.
    pub async fn count_symbol_contained(
        &self,
        symbol_id: i64,
        cap: usize,
        eph: &EphContext,
    ) -> Result<(usize, bool)> {
        let vis = eph.visible_ids();
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let row: CountRow = diesel::sql_query(
            "SELECT count(*) AS n FROM ( \
               SELECT 1 \
               FROM ( \
                 SELECT si.id, si.object_id, si.offset_range, si.dfs_enter, si.dfs_exit \
                 FROM index.symbol_instances si \
                 WHERE si.symbol = $1 AND si.layer = ANY($2) \
                 ORDER BY si.id \
               ) from_inst \
               CROSS JOIN LATERAL ( \
                 SELECT ci.id FROM index.symbol_instances ci \
                 JOIN index.symbols cs ON ci.symbol = cs.id \
                 WHERE ci.object_id = from_inst.object_id \
                   AND from_inst.dfs_enter < ci.dfs_enter \
                   AND ci.dfs_enter < from_inst.dfs_exit \
                   AND ci.layer = ANY($2) AND cs.layer = ANY($2) \
                 ORDER BY ci.id \
               ) ci \
               LIMIT $3 \
             ) c",
        )
        .bind::<BigInt, _>(symbol_id)
        .bind::<Array<BigInt>, _>(&vis)
        .bind::<BigInt, _>(cap as i64 + 1)
        .get_result(&mut *connection)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to count contained instances: {}", e))?;

        let n = row.n as usize;
        Ok((n.min(cap), n > cap))
    }
}
