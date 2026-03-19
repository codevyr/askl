// @generated automatically by Diesel CLI.

diesel::table! {
    use diesel::sql_types::*;

    index.symbol_instances (id) {
        id -> Integer,
        symbol -> Integer,
        object_id -> Integer,
        symbol_type -> Integer,
        offset_range -> Int4range,
    }
}

diesel::table! {
    index.object_contents (object_id) {
        object_id -> Integer,
        content -> Binary,
    }
}

diesel::table! {
    use diesel::sql_types::*;

    index.objects (id) {
        id -> Integer,
        project_id -> Integer,
        module -> Nullable<Integer>,
        directory_id -> Integer,
        module_path -> Text,
        filesystem_path -> Text,
        filetype -> Text,
        content_hash -> Text,
    }
}

diesel::table! {
    index.directories (id) {
        id -> Integer,
        project_id -> Integer,
        parent_id -> Nullable<Integer>,
        path -> Text,
    }
}

diesel::table! {
    index.modules (id) {
        id -> Integer,
        module_name -> Text,
        project_id -> Integer,
    }
}

diesel::table! {
    index.projects (id) {
        id -> Integer,
        project_name -> Text,
        root_path -> Text,
    }
}

diesel::table! {
    use diesel::sql_types::*;

    index.symbol_refs (id) {
        id -> Integer,
        to_symbol -> Integer,
        from_object -> Integer,
        from_offset_range -> Int4range,
    }
}

diesel::table! {
    use diesel::sql_types::*;
    use crate::ltree::Ltree;

    index.symbols (id) {
        id -> Integer,
        name -> Text,
        symbol_path -> Ltree,
        module -> Integer,
        symbol_scope -> Integer,
    }
}

diesel::joinable!(symbol_instances -> objects (object_id));
diesel::joinable!(symbol_instances -> symbols (symbol));
diesel::joinable!(object_contents -> objects (object_id));
diesel::joinable!(objects -> directories (directory_id));
diesel::joinable!(objects -> modules (module));
diesel::joinable!(objects -> projects (project_id));
diesel::joinable!(directories -> projects (project_id));
diesel::joinable!(modules -> projects (project_id));
diesel::joinable!(symbol_refs -> symbols (to_symbol));
diesel::joinable!(symbols -> modules (module));

diesel::allow_tables_to_appear_in_same_query!(
    symbol_instances,
    directories,
    object_contents,
    objects,
    modules,
    projects,
    symbol_refs,
    symbols,
);
