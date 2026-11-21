DROP INDEX index.symbol_refs_layer_uq;

CREATE UNIQUE INDEX symbol_refs_layer_uq
    ON index.symbol_refs (to_symbol, from_object, from_offset_range, layer, dfs_enter);
