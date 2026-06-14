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
        level -> Integer,
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
        eph_layer -> Nullable<BigInt>,
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
    use diesel::sql_types::*;

    index.symbol_refs (id) {
        id -> BigInt,
        to_symbol -> BigInt,
        from_object -> Integer,
        from_offset_range -> Int4range,
        eph_layer -> Nullable<BigInt>,
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
        eph_layer -> Nullable<BigInt>,
    }
}

diesel::table! {
    index.eph_layers (id) {
        id -> BigInt,
        parent_id -> Nullable<BigInt>,
        hash -> Binary,
        kind -> Text,
        last_used -> Timestamptz,
        populated -> Bool,
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
    eph_layers,
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
    symbol_refs,
    symbols,
);
