-- Restore the non-cascading FK (the 2026-08-02 form).
ALTER TABLE index.objects
    DROP CONSTRAINT IF EXISTS objects_layer_fkey,
    ADD CONSTRAINT objects_layer_fkey
        FOREIGN KEY (layer) REFERENCES index.layers(id);
