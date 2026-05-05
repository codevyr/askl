use diesel::prelude::*;
use diesel::OptionalExtension;
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use index::models_diesel::ContentRow;
use index::schema_diesel as index_schema;
use index::symbols::FileId;

use super::{
    ChildCountsRow, DirectoryChildRow, ExistsRow, FileChildRow, IndexStore, NameRow,
    ProjectDetails, ProjectInfo, ProjectTreeNode, ProjectTreeResult, StoreError, UploadStatus,
    normalize_full_path, path_basename,
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
        let n = diesel::sql_query(
            "DELETE FROM index.symbol_refs \
             WHERE to_symbol >= $1::bigint << 32 AND to_symbol < ($1::bigint + 1) << 32",
        )
        .bind::<diesel::sql_types::Integer, _>(project_id)
        .execute(&mut conn)
        .await?;
        tracing::info!(project_id, rows = n, "delete_project: symbol_refs done");

        let n = diesel::sql_query(
            "DELETE FROM index.symbol_instances \
             WHERE symbol >= $1::bigint << 32 AND symbol < ($1::bigint + 1) << 32",
        )
        .bind::<diesel::sql_types::Integer, _>(project_id)
        .execute(&mut conn)
        .await?;
        tracing::info!(project_id, rows = n, "delete_project: symbol_instances done");

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

    pub async fn list_project_tree(
        &self,
        project_id: i32,
        path: &str,
        compact: bool,
    ) -> Result<ProjectTreeResult, StoreError> {
        let mut conn = self.get_conn().await?;

        let project_status: Option<UploadStatus> = index_schema::projects::table
            .filter(index_schema::projects::id.eq(project_id))
            .select(index_schema::projects::upload_status)
            .first::<UploadStatus>(&mut conn)
            .await
            .optional()?;
        match project_status {
            None => return Ok(ProjectTreeResult::ProjectNotFound),
            Some(s) if s != UploadStatus::Complete => return Ok(ProjectTreeResult::NotReady),
            Some(_) => {}
        }

        let normalized = normalize_full_path(path);

        let dir_symbol = index_schema::symbols::table
            .filter(index_schema::symbols::project_id.eq(project_id))
            .filter(index_schema::symbols::symbol_type.eq(4)) // DIRECTORY
            .filter(index_schema::symbols::name.eq(&normalized))
            .select(index_schema::symbols::id)
            .first::<i64>(&mut conn)
            .await
            .optional()?;

        if dir_symbol.is_none() && normalized != "/" {
            return Ok(ProjectTreeResult::NotDirectory);
        }

        let (directories, files) = load_tree_children(
            &mut conn,
            project_id,
            &normalized,
            compact,
        )
        .await?;

        let mut nodes = Vec::with_capacity(directories.len() + files.len());
        for row in directories {
            let name = path_basename(&row.path);
            nodes.push(ProjectTreeNode {
                name,
                path: row.path,
                node_type: "dir".to_string(),
                has_children: row.has_children,
                file_id: None,
                filetype: None,
                compact_path: row.compact_path,
            });
        }

        for row in files {
            let name = path_basename(&row.path);
            nodes.push(ProjectTreeNode {
                name,
                path: row.path,
                node_type: "file".to_string(),
                has_children: false,
                file_id: Some(FileId::new(row.id)),
                filetype: Some(row.filetype),
                compact_path: None,
            });
        }

        nodes.sort_by(|a, b| {
            let a_is_dir = a.node_type == "dir";
            let b_is_dir = b.node_type == "dir";
            b_is_dir.cmp(&a_is_dir).then_with(|| a.path.cmp(&b.path))
        });
        Ok(ProjectTreeResult::Nodes(nodes))
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

/// Load direct child directories and files for a given parent path.
async fn load_tree_children(
    conn: &mut AsyncPgConnection,
    project_id: i32,
    parent_path: &str,
    compact: bool,
) -> Result<(Vec<DirectoryChildRow>, Vec<FileChildRow>), StoreError> {
    let prefix = if parent_path == "/" {
        "/".to_string()
    } else {
        format!("{}/", parent_path)
    };

    let child_dir_names: Vec<String> = diesel::sql_query(
        r#"
        SELECT s.name
        FROM index.symbols s
        WHERE s.project_id = $1
          AND s.symbol_type = 4
          AND starts_with(s.name, $2)
          AND s.name != $2
          AND position('/' IN substring(s.name FROM length($2) + 1)) = 0
        ORDER BY s.name
        "#,
    )
    .bind::<diesel::sql_types::Integer, _>(project_id)
    .bind::<diesel::sql_types::Text, _>(&prefix)
    .load::<NameRow>(conn)
    .await?
    .into_iter()
    .map(|r| r.name)
    .collect();

    let mut dir_children = Vec::with_capacity(child_dir_names.len());
    for dir_name in &child_dir_names {
        let child_prefix = format!("{}/", dir_name);
        let counts = query_child_counts(conn, project_id, &child_prefix).await?;
        let has_children = counts.dir_count > 0 || counts.file_count > 0;

        let compact_path = if compact && counts.dir_count == 1 && counts.file_count == 0 {
            compute_compact_path(conn, project_id, dir_name).await?
        } else {
            None
        };

        dir_children.push(DirectoryChildRow {
            path: dir_name.clone(),
            has_children,
            compact_path,
        });
    }

    let files = load_file_children(conn, project_id, &prefix).await?;

    Ok((dir_children, files))
}

/// Query direct child dir count and file count under a prefix.
async fn query_child_counts(
    conn: &mut AsyncPgConnection,
    project_id: i32,
    child_prefix: &str,
) -> Result<ChildCountsRow, StoreError> {
    let row = diesel::sql_query(
        r#"
        SELECT
            (SELECT COUNT(*) FROM index.symbols s
             WHERE s.project_id = $1 AND s.symbol_type = 4
               AND starts_with(s.name, $2)
               AND position('/' IN substring(s.name FROM length($2) + 1)) = 0
            ) AS dir_count,
            (SELECT COUNT(*) FROM index.symbols s
             WHERE s.project_id = $1 AND s.symbol_type = 2
               AND starts_with(s.name, $2)
               AND position('/' IN substring(s.name FROM length($2) + 1)) = 0
            ) AS file_count
        "#,
    )
    .bind::<diesel::sql_types::Integer, _>(project_id)
    .bind::<diesel::sql_types::Text, _>(child_prefix)
    .get_result::<ChildCountsRow>(conn)
    .await?;
    Ok(row)
}

/// Walk down a chain of single-child-no-files directories for compact display.
async fn compute_compact_path(
    conn: &mut AsyncPgConnection,
    project_id: i32,
    dir_path: &str,
) -> Result<Option<String>, StoreError> {
    let mut current = dir_path.to_string();
    for _ in 0..20 {
        let child_prefix = format!("{}/", current);

        let child_dirs: Vec<NameRow> = diesel::sql_query(
            r#"
            SELECT s.name
            FROM index.symbols s
            WHERE s.project_id = $1
              AND s.symbol_type = 4
              AND starts_with(s.name, $2)
              AND position('/' IN substring(s.name FROM length($2) + 1)) = 0
            LIMIT 2
            "#,
        )
        .bind::<diesel::sql_types::Integer, _>(project_id)
        .bind::<diesel::sql_types::Text, _>(&child_prefix)
        .load(conn)
        .await?;

        if child_dirs.len() != 1 {
            break;
        }

        let has_files = diesel::sql_query(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM index.symbols s
                WHERE s.project_id = $1
                  AND s.symbol_type = 2
                  AND starts_with(s.name, $2)
                  AND position('/' IN substring(s.name FROM length($2) + 1)) = 0
            ) AS exists
            "#,
        )
        .bind::<diesel::sql_types::Integer, _>(project_id)
        .bind::<diesel::sql_types::Text, _>(&child_prefix)
        .get_result::<ExistsRow>(conn)
        .await?;

        current = child_dirs.into_iter().next().unwrap().name;
        if has_files.exists {
            break;
        }
    }

    if current != dir_path {
        Ok(Some(current))
    } else {
        Ok(None)
    }
}

/// Load direct child files under a parent prefix.
async fn load_file_children(
    conn: &mut AsyncPgConnection,
    project_id: i32,
    parent_prefix: &str,
) -> Result<Vec<FileChildRow>, StoreError> {
    let rows = diesel::sql_query(
        r#"
        SELECT DISTINCT o.id, o.filesystem_path AS path, o.filetype
        FROM index.objects o
        JOIN index.symbols fs ON fs.name = o.filesystem_path
        WHERE fs.project_id = $1
          AND fs.symbol_type = 2
          AND starts_with(fs.name, $2)
          AND position('/' IN substring(fs.name FROM length($2) + 1)) = 0
        ORDER BY o.filesystem_path
        "#,
    )
    .bind::<diesel::sql_types::Integer, _>(project_id)
    .bind::<diesel::sql_types::Text, _>(parent_prefix)
    .load::<FileChildRow>(conn)
    .await?;
    Ok(rows)
}
