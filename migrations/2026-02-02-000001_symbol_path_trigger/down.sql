DROP TRIGGER IF EXISTS symbols_set_path ON index.symbols;
DROP FUNCTION IF EXISTS index.set_symbol_path();
DROP FUNCTION IF EXISTS index.symbol_name_to_ltree(text);
