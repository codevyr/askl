-- Reverting re-forbids two references to one target on one token, so an index
-- produced after this migration may not fit.  Nothing here can repair that;
-- the rows have to be re-uploaded from a producer that keeps one of them.

DROP INDEX index.symbol_refs_layer_uq;

CREATE UNIQUE INDEX symbol_refs_layer_uq
    ON index.symbol_refs (to_symbol, from_object, from_offset_range, layer);
