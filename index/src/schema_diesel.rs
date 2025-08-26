// @generated automatically by Diesel CLI.

diesel::table! {
    declarations (id) {
        id -> Integer,
        symbol -> Integer,
        file_id -> Integer,
        symbol_type -> Integer,
        line_start -> Integer,
        col_start -> Integer,
        line_end -> Integer,
        col_end -> Integer,
    }
}

diesel::table! {
    file_contents (file_id) {
        file_id -> Integer,
        content -> Binary,
    }
}

diesel::table! {
    files (id) {
        id -> Integer,
        module -> Integer,
        module_path -> Text,
        filesystem_path -> Text,
        filetype -> Text,
    }
}

diesel::table! {
    modules (id) {
        id -> Integer,
        module_name -> Text,
    }
}

diesel::table! {
    symbol_refs (rowid) {
        rowid -> Integer,
        from_decl -> Integer,
        to_symbol -> Integer,
        from_line -> Integer,
        from_col_start -> Integer,
        from_col_end -> Integer,
    }
}

diesel::table! {
    symbols (id) {
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
diesel::joinable!(symbol_refs -> declarations (from_decl));
diesel::joinable!(symbol_refs -> symbols (to_symbol));
diesel::joinable!(symbols -> modules (module));

diesel::allow_tables_to_appear_in_same_query!(
    declarations,
    file_contents,
    files,
    modules,
    symbol_refs,
    symbols,
);
