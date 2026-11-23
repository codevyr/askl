// @generated automatically by Diesel CLI.

diesel::table! {
    index.instance_types (id) {
        id -> Integer,
        name -> Text,
    }
}

diesel::table! {
    index.symbol_types (id) {
        id -> Integer,
        name -> Text,
        dot_is_separator -> Bool,
    }
}

diesel::table! {
    use diesel::sql_types::*;

    index.symbol_instances (id) {
        id -> BigInt,
        symbol -> BigInt,
        object_id -> Integer,
        offset_range -> Int4range,
        instance_type -> Integer,
        layer -> BigInt,
        nesting_level -> Integer,
        dfs_enter -> BigInt,
        dfs_exit -> BigInt,
        name_range -> Nullable<Int4range>,
        signature_range -> Nullable<Int4range>,
        content_range -> Nullable<Int4range>,
    }
}

diesel::table! {
    index.object_contents (object_id) {
        object_id -> Integer,
        content -> Binary,
    }
}

diesel::table! {
    index.content_store (content_hash) {
        content_hash -> Text,
        content -> Binary,
    }
}

diesel::table! {
    use diesel::sql_types::*;

    index.objects (id) {
        id -> Integer,
        project_id -> Integer,
        // directory_id removed - directories are now symbols
        module_path -> Text,
        filesystem_path -> Text,
        filetype -> Text,
        content_hash -> Text,
        // Directory sentinel objects have:
        // - filesystem_path = directory path (e.g., "/src")
        // - filetype = "directory"
        // - content_hash = "" (empty)
        layer -> BigInt,
        // The id the upload gave this object; lets a witness in another
        // chunk of the upload name it.  NULL for an object no upload made.
        local_id -> Nullable<BigInt>,
    }
}

// directories table has been removed - directories are now symbols

diesel::table! {
    index.projects (id) {
        id -> Integer,
        project_name -> Text,
        root_path -> Text,
        upload_status -> Text,
        symbol_chunks_total -> Nullable<Integer>,
        object_chunks_total -> Nullable<Integer>,
        root_layer_id -> BigInt,
        // What the upload was observed to state (set at ingest, never
        // cleared): see `index::db_diesel::ProjectStates`.
        states_roles -> Bool,
        has_declares -> Bool,
        has_holds -> Bool,
    }
}

diesel::table! {
    index.project_symbol_chunks (project_id, seq) {
        project_id -> Integer,
        seq -> Integer,
    }
}

diesel::table! {
    index.project_object_chunks (project_id, seq) {
        project_id -> Integer,
        seq -> Integer,
    }
}

diesel::table! {
    // Objects that the witnesses of an upload in progress name from another
    // chunk; checked and emptied at finalize.  It has no key of its own: a
    // chunk adds the objects it could not see, and two chunks may add the
    // same one.
    index.project_witness_objects (project_id, object_local_id) {
        project_id -> Integer,
        object_local_id -> BigInt,
    }
}

diesel::table! {
    use diesel::sql_types::*;

    index.symbol_refs (id) {
        id -> BigInt,
        to_symbol -> BigInt,
        from_object -> Integer,
        from_offset_range -> Int4range,
        layer -> BigInt,
        nesting_level -> Integer,
        dfs_enter -> BigInt,
        from_full_range -> Nullable<Int4range>,
        roles -> SmallInt,
        relation -> SmallInt,
        // Where a relation happened: one place in the code that made it,
        // its object named by `objects.local_id`, and how many there are.
        // All three NULL on a reference.
        witness_object_local_id -> Nullable<BigInt>,
        witness_range -> Nullable<Int4range>,
        witness_sites -> Nullable<Integer>,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use crate::ltree::Ltree;

    index.symbols (id) {
        id -> BigInt,
        name -> Text,
        symbol_path -> Ltree,
        project_id -> Integer,
        symbol_type -> Integer,
        symbol_scope -> Nullable<Integer>,
        leaf_name -> Text,
        layer -> BigInt,
        folded -> Bool,
    }
}

diesel::table! {
    index.layers (id) {
        id -> BigInt,
        parent_id -> Nullable<BigInt>,
        hash -> Binary,
        kind -> Text,
        last_used -> Timestamptz,
        populated -> Bool,
        truncated -> Bool,
        root_shard_id -> Nullable<BigInt>,
    }
}

diesel::joinable!(symbol_instances -> instance_types (instance_type));
diesel::joinable!(symbol_instances -> objects (object_id));
diesel::joinable!(symbol_instances -> symbols (symbol));
diesel::joinable!(object_contents -> objects (object_id));
// joinable!(objects -> directories (directory_id)) removed - directories table dropped
diesel::joinable!(objects -> projects (project_id));
// joinable!(directories -> projects (project_id)) removed - directories table dropped
diesel::joinable!(symbol_refs -> symbols (to_symbol));
diesel::joinable!(symbols -> projects (project_id));
diesel::joinable!(symbols -> symbol_types (symbol_type));
diesel::allow_tables_to_appear_in_same_query!(
    layers,
    instance_types,
    symbol_instances,
    symbol_types,
    // directories removed
    content_store,
    object_contents,
    objects,
    projects,
    project_symbol_chunks,
    project_object_chunks,
    project_witness_objects,
    symbol_refs,
    symbols,
);
