-- Reverting re-forbids two instances of one symbol at one extent, so an index
-- produced after this migration may not fit.  Nothing here can repair that;
-- the rows have to be re-uploaded from a producer that collapses them.

DROP INDEX index.symbol_instances_layer_uq;

CREATE UNIQUE INDEX symbol_instances_layer_uq
    ON index.symbol_instances (symbol, object_id, offset_range, layer);
