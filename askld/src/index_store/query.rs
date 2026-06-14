use diesel::prelude::*;
use diesel::OptionalExtension;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use std::collections::HashMap;

use index::models_diesel::ContentRow;
use index::schema_diesel as index_schema;
use index::symbols::{FileId, SymbolType};

use super::{
    normalize_full_path, path_basename, BatchedDirRow, BatchedFileRow, CompactableRow, IndexStore,
    MultiTreeResult, NodeType, ProjectDetails, ProjectInfo, ProjectTreeNode, StoreError,
    UploadStatus,
};

impl IndexStore {
    pub async fn list_projects(&self) -> Result<Vec<ProjectInfo>, StoreError> {
        let mut conn = self.get_conn().await?;
        let rows: Vec<(i32, String, String, UploadStatus)> = index_schema::projects::table
            .select((
                index_schema::projects::id,
                index_schema::projects::project_name,
                index_schema::projects::root_path,
                index_schema::projects::upload_status,
            ))
            .order(index_schema::projects::id)
            .load(&mut conn)
            .await?;
        Ok(rows
            .into_iter()
            .map(|(id, project_name, root_path, upload_status)| ProjectInfo {
                id,
                project_name,
                root_path,
                upload_status,
            })
            .collect())
    }

    pub async fn get_project_details(
        &self,
        project_id: i32,
    ) -> Result<Option<ProjectDetails>, StoreError> {
        let mut conn = self.get_conn().await?;

        let project_row: Option<(i32, String, String, UploadStatus, Option<i32>, Option<i32>)> =
            index_schema::projects::table
                .filter(index_schema::projects::id.eq(project_id))
                .select((
                    index_schema::projects::id,
                    index_schema::projects::project_name,
                    index_schema::projects::root_path,
                    index_schema::projects::upload_status,
                    index_schema::projects::symbol_chunks_total,
                    index_schema::projects::object_chunks_total,
                ))
                .first(&mut conn)
                .await
                .optional()?;

        let (id, project_name, root_path, upload_status, symbol_chunks_total, object_chunks_total) =
            match project_row {
                Some(row) => row,
                None => return Ok(None),
            };

        let file_count: i64 = index_schema::objects::table
            .filter(index_schema::objects::project_id.eq(project_id))
            .count()
            .get_result(&mut conn)
            .await?;

        let symbol_count: i64 = index_schema::symbols::table
            .filter(index_schema::symbols::project_id.eq(project_id))
            .count()
            .get_result(&mut conn)
            .await?;

        let committed_symbol_chunks: Vec<i32> = index_schema::project_symbol_chunks::table
            .filter(index_schema::project_symbol_chunks::project_id.eq(project_id))
            .select(index_schema::project_symbol_chunks::seq)
            .order(index_schema::project_symbol_chunks::seq)
            .load(&mut conn)
            .await?;

        let committed_object_chunks: Vec<i32> = index_schema::project_object_chunks::table
            .filter(index_schema::project_object_chunks::project_id.eq(project_id))
            .select(index_schema::project_object_chunks::seq)
            .order(index_schema::project_object_chunks::seq)
            .load(&mut conn)
            .await?;

        Ok(Some(ProjectDetails {
            id,
            project_name,
            root_path,
            upload_status,
            file_count,
            symbol_count,
            symbol_chunks_total,
            object_chunks_total,
            committed_symbol_chunks,
            committed_object_chunks,
        }))
    }

    /// Delete a project and all its data.
    ///
    /// Proceeds in dependency order to avoid per-row ON DELETE CASCADE overhead:
    ///
    /// 1. Mark `Deleting` — reserves the name; a crash at any later step leaves a zombie
    ///    that the next re-upload of the same name will clean up automatically.
    /// 2. Delete `symbol_refs` and `symbol_instances` using a range scan on the global
    ///    symbol ID: every symbol for project P has `id = P << 32 | local_id`, so all of
    ///    a project's symbols occupy `[P << 32, (P+1) << 32)` in the B-tree index.
    ///    This avoids a subquery join through `symbols` entirely.
    /// 3. Delete `symbols` — CASCADE to the now-empty instances/refs is a no-op.
    /// 4. Delete `objects` — CASCADE handles `object_contents` via its PK (fast 1:1).
    /// 5. Delete the `projects` row.
    pub async fn delete_project(&self, project_id: i32) -> Result<bool, StoreError> {
        let mut conn = self.get_conn().await?;

        // Mark as deleting first so the name is immediately reserved and any crash
        // leaves a zombie that the next re-upload of the same name will clean up.
        let marked = diesel::update(
            index_schema::projects::table.filter(index_schema::projects::id.eq(project_id)),
        )
        .set(index_schema::projects::upload_status.eq(UploadStatus::Deleting))
        .execute(&mut conn)
        .await?;
        if marked == 0 {
            return Ok(false);
        }
        tracing::info!(project_id, "delete_project: marked Deleting");

        // Global symbol IDs encode the project: symbol = project_id << 32 | local_id.
        // All symbols for a project form a contiguous range in the B-tree — one range
        // scan on symbol_refs_to_symbol_idx/symbol_instances_symbol_idx, no subquery.
        let lower = super::upload::project_symbol_id_base(project_id);
        let upper = super::upload::project_symbol_id_base(project_id + 1);
        let n = diesel::delete(
            index_schema::symbol_refs::table
                .filter(index_schema::symbol_refs::to_symbol.ge(lower))
                .filter(index_schema::symbol_refs::to_symbol.lt(upper)),
        )
        .execute(&mut conn)
        .await?;
        tracing::info!(project_id, rows = n, "delete_project: symbol_refs done");

        let n = diesel::delete(
            index_schema::symbol_instances::table
                .filter(index_schema::symbol_instances::symbol.ge(lower))
                .filter(index_schema::symbol_instances::symbol.lt(upper)),
        )
        .execute(&mut conn)
        .await?;
        tracing::info!(
            project_id,
            rows = n,
            "delete_project: symbol_instances done"
        );

        let n = diesel::delete(
            index_schema::symbols::table.filter(index_schema::symbols::project_id.eq(project_id)),
        )
        .execute(&mut conn)
        .await?;
        tracing::info!(project_id, rows = n, "delete_project: symbols done");

        let n = diesel::delete(
            index_schema::objects::table.filter(index_schema::objects::project_id.eq(project_id)),
        )
        .execute(&mut conn)
        .await?;
        tracing::info!(project_id, rows = n, "delete_project: objects done");

        // Deleting the project row cascades any remaining ON DELETE CASCADE children.
        diesel::delete(
            index_schema::projects::table.filter(index_schema::projects::id.eq(project_id)),
        )
        .execute(&mut conn)
        .await?;
        tracing::info!(project_id, "delete_project: complete");

        Ok(true)
    }

    #[tracing::instrument(skip(self), fields(path_count = paths.len()))]
    pub async fn list_project_tree_multi(
        &self,
        project_id: i32,
        paths: &[String],
        compact: bool,
    ) -> Result<MultiTreeResult, StoreError> {
        if paths.is_empty() {
            return Ok(MultiTreeResult::Nodes(HashMap::new()));
        }

        let mut conn = self.get_conn().await?;

        let project_status: Option<UploadStatus> = index_schema::projects::table
            .filter(index_schema::projects::id.eq(project_id))
            .select(index_schema::projects::upload_status)
            .first::<UploadStatus>(&mut conn)
            .await
            .optional()?;
        match project_status {
            None => return Ok(MultiTreeResult::ProjectNotFound),
            Some(s) if s != UploadStatus::Complete => return Ok(MultiTreeResult::NotReady),
            Some(_) => {}
        }

        let normalized: Vec<String> = paths.iter().map(|p| normalize_full_path(p)).collect();

        // Validate all non-root paths are directories.
        let non_root: Vec<String> = normalized
            .iter()
            .filter(|p| p.as_str() != "/")
            .cloned()
            .collect();
        if !non_root.is_empty() {
            let found: std::collections::HashSet<String> = index_schema::symbols::table
                .filter(index_schema::symbols::project_id.eq(project_id))
                .filter(index_schema::symbols::symbol_type.eq(SymbolType::Directory as i32))
                .filter(index_schema::symbols::name.eq_any(&non_root))
                .select(index_schema::symbols::name)
                .load::<String>(&mut conn)
                .await?
                .into_iter()
                .collect();

            if let Some(invalid) = non_root.into_iter().find(|p| !found.contains(p)) {
                return Ok(MultiTreeResult::NotDirectory(invalid));
            }
        }

        let (dir_rows, file_rows) =
            load_tree_children_multi(&mut conn, project_id, &normalized, compact).await?;

        // Group results by original normalized path using the prefix→path mapping.
        let prefix_to_path: HashMap<String, String> = normalized
            .iter()
            .map(|p| {
                let prefix = if p == "/" {
                    "/".to_string()
                } else {
                    format!("{}/", p)
                };
                (prefix, p.clone())
            })
            .collect();

        let mut result: HashMap<String, Vec<ProjectTreeNode>> =
            normalized.iter().map(|p| (p.clone(), Vec::new())).collect();

        for row in dir_rows {
            let parent = match prefix_to_path.get(&row.parent_prefix) {
                Some(p) => p.clone(),
                None => continue,
            };
            let name = path_basename(&row.path);
            result.entry(parent).or_default().push(ProjectTreeNode {
                name,
                path: row.path,
                node_type: NodeType::Dir,
                has_children: row.has_children,
                file_id: None,
                filetype: None,
                compact_path: row.compact_path,
            });
        }

        for row in file_rows {
            let parent = match prefix_to_path.get(&row.parent_prefix) {
                Some(p) => p.clone(),
                None => continue,
            };
            let name = path_basename(&row.path);
            result.entry(parent).or_default().push(ProjectTreeNode {
                name,
                path: row.path,
                node_type: NodeType::File,
                has_children: false,
                file_id: Some(FileId::new(row.id)),
                filetype: Some(row.filetype),
                compact_path: None,
            });
        }

        for nodes in result.values_mut() {
            nodes.sort_by(|a, b| {
                let a_is_dir = a.node_type == NodeType::Dir;
                let b_is_dir = b.node_type == NodeType::Dir;
                b_is_dir.cmp(&a_is_dir).then_with(|| a.path.cmp(&b.path))
            });
        }

        Ok(MultiTreeResult::Nodes(result))
    }

    pub async fn get_project_file_contents_by_path(
        &self,
        project_id: i32,
        path: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let mut conn = self.get_conn().await?;

        let normalized = normalize_full_path(path);
        let content: Option<Vec<u8>> = diesel::sql_query(
            r#"
            SELECT COALESCE(oc.content, cs.content) AS content
            FROM index.objects o
            LEFT JOIN index.object_contents oc ON oc.object_id = o.id
            LEFT JOIN index.content_store cs ON cs.content_hash = o.content_hash
            WHERE o.project_id = $1
              AND o.filesystem_path = $2
            LIMIT 1
            "#,
        )
        .bind::<diesel::sql_types::Integer, _>(project_id)
        .bind::<diesel::sql_types::Text, _>(normalized)
        .get_result::<ContentRow>(&mut conn)
        .await
        .optional()?
        .map(|row| row.content);

        Ok(content)
    }

    /// Return which of the given content hashes already exist in the content store.
    /// Used by clients to skip uploading content that is already present.
    pub async fn check_content_hashes(
        &self,
        hashes: Vec<String>,
    ) -> Result<Vec<String>, StoreError> {
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        let mut conn = self.get_conn().await?;
        let present: Vec<String> = index_schema::content_store::table
            .filter(index_schema::content_store::content_hash.eq_any(&hashes))
            .select(index_schema::content_store::content_hash)
            .load(&mut conn)
            .await?;
        Ok(present)
    }
}

/// Fetch all single-child-no-files directories for the project in one query.
/// Returns a map of parent_path → child_path used to walk compact chains in Rust.
///
/// Identifies compactable directories purely by path-string arithmetic:
/// for each file/dir symbol (excluding root), compute its parent by stripping
/// the last path component, then group by parent. A parent is compactable iff
/// it has exactly one direct child and that child is a directory.
#[tracing::instrument(skip(conn))]
async fn query_compactable_dirs(
    conn: &mut AsyncPgConnection,
    project_id: i32,
) -> Result<HashMap<String, String>, StoreError> {
    let rows: Vec<CompactableRow> = diesel::sql_query(
        r#"
        SELECT
            parent_name,
            MIN(child_name) FILTER (WHERE symbol_type = $2) AS child_name
        FROM (
            SELECT
                s.name AS child_name,
                s.symbol_type,
                CASE WHEN position('/' IN ltrim(s.name, '/')) = 0 THEN '/'
                     ELSE left(s.name, length(s.name) - position('/' IN reverse(s.name)))
                END AS parent_name
            FROM index.symbols s
            WHERE s.project_id = $1
              AND (s.symbol_type = $2 OR s.symbol_type = $3)
              AND s.name != '/'
        ) children
        GROUP BY parent_name
        HAVING count(*) = 1
           AND count(*) FILTER (WHERE symbol_type = $2) = 1
        "#,
    )
    .bind::<diesel::sql_types::Integer, _>(project_id)
    .bind::<diesel::sql_types::Integer, _>(SymbolType::Directory as i32)
    .bind::<diesel::sql_types::Integer, _>(SymbolType::File as i32)
    .load(conn)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| (r.parent_name, r.child_name))
        .collect())
}

/// Walk the compactable map to find the terminal path of a compact chain.
///
/// The chain terminates when a path has no single-child compactable successor.
/// As a cycle-safety bound we stop once the accumulated path length exceeds
/// the POSIX PATH_MAX of 4096 bytes; a legitimate filesystem path cannot be
/// longer than that.
fn walk_compact_chain(start: &str, map: &HashMap<String, String>) -> Option<String> {
    let mut current = start;
    while current.len() <= 4096 {
        match map.get(current) {
            Some(next) => current = next.as_str(),
            None => break,
        }
    }
    if current != start {
        Some(current.to_string())
    } else {
        None
    }
}

/// Load children for multiple parent paths in two batch queries (dirs + files).
#[tracing::instrument(skip(conn), fields(path_count = paths.len()))]
async fn load_tree_children_multi(
    conn: &mut AsyncPgConnection,
    project_id: i32,
    paths: &[String],
    compact: bool,
) -> Result<(Vec<BatchedDirRow>, Vec<BatchedFileRow>), StoreError> {
    // Deduplicate: repeated prefixes (e.g. "/" appearing for both base path and an
    // expand path) would make unnest emit duplicate parent_prefix values, causing
    // each child node to appear twice in the results.
    let mut seen_prefixes = std::collections::HashSet::new();
    let prefixes: Vec<String> = paths
        .iter()
        .map(|p| {
            if p == "/" {
                "/".to_string()
            } else {
                format!("{}/", p)
            }
        })
        .filter(|p| seen_prefixes.insert(p.clone()))
        .collect();

    // The main join uses nlevel(symbol_path) = children_nlevel to restrict the
    // index scan to exactly the right depth layer.  children_nlevel is the number
    // of '/' characters in the prefix string.
    //
    // has_children is computed in bulk via two extra CTEs (one for directory
    // grandchildren, one for file grandchildren), each scanning once per prefix
    // rather than once per returned directory.  This replaces ~N×2 correlated
    // EXISTS subqueries with 2×|prefixes| range scans.
    let mut dir_rows: Vec<BatchedDirRow> = diesel::sql_query(
        r#"
        WITH prefixes AS (
            SELECT
                t.prefix,
                length(t.prefix) - length(replace(t.prefix, '/', '')) AS children_nlevel
            FROM unnest($2::text[]) AS t(prefix)
        ),
        direct_dirs AS (
            SELECT s.name AS path, p.prefix AS parent_prefix
            FROM prefixes p
            JOIN index.symbols s ON s.project_id = $1
              AND s.symbol_type = $3
              AND nlevel(s.symbol_path) = p.children_nlevel
              AND starts_with(s.name, p.prefix)
              AND s.name != p.prefix
        ),
        dirs_with_dir_children AS (
            SELECT DISTINCT
                left(c.name, length(c.name) - position('/' IN reverse(c.name))) AS parent_name
            FROM prefixes p
            JOIN index.symbols c ON c.project_id = $1
              AND c.symbol_type = $3
              AND nlevel(c.symbol_path) = p.children_nlevel + 1
              AND starts_with(c.name, p.prefix)
        ),
        dirs_with_file_children AS (
            SELECT DISTINCT
                left(c.name, length(c.name) - position('/' IN reverse(c.name))) AS parent_name
            FROM prefixes p
            JOIN index.symbols c ON c.project_id = $1
              AND c.symbol_type = $4
              AND nlevel(c.symbol_path) = p.children_nlevel + 1
              AND starts_with(c.name, p.prefix)
        )
        SELECT
            d.path,
            d.parent_prefix,
            (ddc.parent_name IS NOT NULL OR dfc.parent_name IS NOT NULL) AS has_children,
            NULL::text AS compact_path
        FROM direct_dirs d
        LEFT JOIN dirs_with_dir_children ddc ON ddc.parent_name = d.path
        LEFT JOIN dirs_with_file_children dfc ON dfc.parent_name = d.path
        ORDER BY d.parent_prefix, d.path
        "#,
    )
    .bind::<diesel::sql_types::Integer, _>(project_id)
    .bind::<diesel::sql_types::Array<diesel::sql_types::Text>, _>(&prefixes)
    .bind::<diesel::sql_types::Integer, _>(SymbolType::Directory as i32)
    .bind::<diesel::sql_types::Integer, _>(SymbolType::File as i32)
    .load(conn)
    .await?;

    if compact {
        let compactable = query_compactable_dirs(conn, project_id).await?;
        for row in &mut dir_rows {
            row.compact_path = walk_compact_chain(&row.path, &compactable);
        }
    }

    // file_nlevel = slash count in prefix = nlevel of direct children.
    // nlevel(symbol_path) = file_nlevel restricts the index scan to exactly the
    // right depth layer, using symbols_project_type_nlevel_name_idx.
    //
    // The join to objects goes via symbol_instances on IDs rather than matching
    // on filesystem_path strings.  File symbols (symbol_type=2) only ever have
    // file-content instances (source/header/build/file); containment and sentinel
    // instance types are used exclusively by Directory symbols, so no instance_type
    // filter is needed here.  DISTINCT guards against any future edge cases where
    // a file symbol might accumulate multiple instances.
    let file_rows: Vec<BatchedFileRow> = diesel::sql_query(
        r#"
        WITH prefixes AS (
            SELECT
                t.prefix,
                length(t.prefix) - length(replace(t.prefix, '/', '')) AS file_nlevel
            FROM unnest($2::text[]) AS t(prefix)
        )
        SELECT DISTINCT o.id, fs.name AS path, o.filetype, p.prefix AS parent_prefix
        FROM prefixes p
        JOIN index.symbols fs ON fs.project_id = $1
          AND fs.symbol_type = $3
          AND nlevel(fs.symbol_path) = p.file_nlevel
          AND starts_with(fs.name, p.prefix)
        JOIN index.symbol_instances si ON si.symbol = fs.id
        JOIN index.objects o ON o.id = si.object_id
        ORDER BY p.prefix, fs.name
        "#,
    )
    .bind::<diesel::sql_types::Integer, _>(project_id)
    .bind::<diesel::sql_types::Array<diesel::sql_types::Text>, _>(&prefixes)
    .bind::<diesel::sql_types::Integer, _>(SymbolType::File as i32)
    .load(conn)
    .await?;

    Ok((dir_rows, file_rows))
}
