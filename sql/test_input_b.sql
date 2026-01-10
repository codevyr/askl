INSERT INTO projects
VALUES (1, "test_project");

INSERT INTO modules
VALUES (1, "test", 1);

INSERT INTO files
VALUES
    (1, 1, "main.c", "/main.c", "cc", ""),
    (2, 1, "bar.c", "/bar.c", "cc", "");


INSERT INTO symbols
VALUES
    (1,  "a",    1, 1),
    (2,  "b",    1, 1),
    (3,  "c",    1, 1),
    (4,  "d",    1, 1),
    (5,  "e",    1, 1),
    (6,  "f",    1, 1),
    (7,  "g",    1, 1),
    (42, "main", 1, 1);

INSERT INTO declarations
VALUES
    (91,  1,  1, 1, 910, 919),
    (92,  2,  1, 1, 920, 929),
    (93,  3,  1, 1, 930, 939),
    (94,  4,  1, 1, 940, 949),
    (95,  5,  1, 1, 950, 959),
    (86,  6,  2, 1, 860, 869),
    (96,  6,  1, 1, 960, 969),
    (97,  7,  1, 1, 970, 979),
    (942, 42, 1, 1, 9420, 9429);

INSERT INTO
    symbol_refs(to_symbol, from_file, from_offset_start, from_offset_end)
VALUES
    (2, 1, 911, 911),
    (4, 1, 921, 921),
    (2, 1, 931, 931),
    (5, 1, 941, 941),
    (6, 1, 942, 942),
    (6, 1, 951, 951),
    (5, 1, 971, 971),
    (1, 1, 9421, 9421),
    (2, 1, 9422, 9422);


-- main -> {a c}
-- {a c} -> b
-- b -> d -> {e f}
-- g -> e -> f
