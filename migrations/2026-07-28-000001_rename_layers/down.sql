ALTER TABLE index.layers RENAME TO eph_layers;
ALTER TABLE index.symbols RENAME COLUMN layer TO eph_layer;
ALTER TABLE index.symbol_instances RENAME COLUMN layer TO eph_layer;
ALTER TABLE index.symbol_refs RENAME COLUMN layer TO eph_layer;
