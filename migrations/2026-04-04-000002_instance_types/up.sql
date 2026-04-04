CREATE TABLE IF NOT EXISTS index.instance_types (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);

INSERT INTO index.instance_types (id, name) VALUES
    (1, 'definition'),
    (2, 'declaration'),
    (3, 'expansion'),
    (4, 'sentinel'),
    (5, 'containment'),
    (6, 'source'),
    (7, 'header'),
    (8, 'build');

ALTER TABLE index.symbol_instances
    ADD COLUMN instance_type INTEGER NOT NULL DEFAULT 1
    REFERENCES index.instance_types(id);

CREATE INDEX IF NOT EXISTS symbol_instances_instance_type_idx
    ON index.symbol_instances (instance_type);
