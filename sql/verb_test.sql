INSERT INTO
    modules
VALUES
    (1, "test");

INSERT INTO
    files
VALUES
    (1, 1, "main.c", "/main.c", "cc");

INSERT INTO
    symbols
VALUES
    (1, "foo", 1, 1),
    (2, "foo.bar", 1, 1),
    (3, "foobar", 1, 1),
    (4, "tar", 1, 1);

INSERT INTO
    declarations
VALUES
    (91, 1, 1, 1, 1, 1, 1, 1),
    (92, 2, 1, 1, 1, 1, 1, 1),
    (93, 3, 1, 1, 1, 1, 1, 1),
    (94, 4, 1, 1, 1, 1, 1, 1);

INSERT INTO
    symbol_refs
VALUES
    (91, 2, 1, 16, 16),
    (91, 2, 1, 22, 22),
    (92, 3, 1, 1, 1),
    (92, 4, 1, 1, 1);