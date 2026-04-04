DELETE FROM index.symbol_types WHERE id = 8;

-- Shift levels back down by 1
UPDATE index.symbol_types SET level = level - 1;
