// @generated automatically by Diesel CLI.

diesel::table! {
    index.declarations (id) {
        id -> Integer,
        symbol -> Integer,
        file_id -> Integer,
        symbol_type -> Integer,
        start_offset -> Integer,
        end_offset -> Integer,
    }
}

diesel::table! {
    index.file_contents (file_id) {
        file_id -> Integer,
        content -> Binary,
    }
}

diesel::table! {
    index.files (id) {
        id -> Integer,
        module -> Integer,
        module_path -> Text,
        filesystem_path -> Text,
        filetype -> Text,
        content_hash -> Text,
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
    }
}

diesel::table! {
    index.symbol_refs (id) {
        id -> Integer,
        to_symbol -> Integer,
        from_file -> Integer,
        from_offset_start -> Integer,
        from_offset_end -> Integer,
    }
}

diesel::table! {
    index.symbols (id) {
        id -> Integer,
        name -> Text,
        module -> Integer,
        symbol_scope -> Integer,
    }
}

diesel::joinable!(declarations -> files (file_id));
diesel::joinable!(declarations -> symbols (symbol));
diesel::joinable!(file_contents -> files (file_id));
diesel::joinable!(files -> modules (module));
diesel::joinable!(modules -> projects (project_id));
diesel::joinable!(symbol_refs -> symbols (to_symbol));
diesel::joinable!(symbols -> modules (module));

diesel::allow_tables_to_appear_in_same_query!(
    declarations,
    file_contents,
    files,
    modules,
    projects,
    symbol_refs,
    symbols,
);
