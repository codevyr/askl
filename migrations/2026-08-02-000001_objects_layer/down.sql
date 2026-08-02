DROP INDEX IF EXISTS index.objects_layer_idx;
ALTER TABLE index.objects
    DROP CONSTRAINT IF EXISTS objects_layer_sign_check,
    DROP CONSTRAINT IF EXISTS objects_layer_fkey,
    DROP COLUMN IF EXISTS layer;
