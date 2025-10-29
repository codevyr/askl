INSERT INTO projects
VALUES
    (1, "test_project"),
    (2, "other_project");

INSERT INTO modules
VALUES
    (1, "test", 1),
    (2, "other", 1),
    (3, "project_only", 2);

INSERT INTO files
VALUES
    (1, 1, "main.c", "/main.c", "cc", ""),
    (2, 1, "bar.c", "/bar.c", "cc", ""),
    (3, 2, "main.c", "/other_main.c", "cc", ""),
    (4, 3, "main.c", "/project_only_main.c", "cc", "");

INSERT INTO symbols
VALUES
    (1,   "a",    1, 1),
    (2,   "b",    1, 1),
    (3,   "c",    1, 1),
    (4,   "d",    1, 1),
    (5,   "e",    1, 1),
    (6,   "f",    1, 1),
    (7,   "g",    1, 1),
    (42,  "main", 1, 1),
    (101, "a",    2, 1),
    (102, "b",    2, 1),
    (142, "main", 2, 1),
    (301, "a",    3, 1);

INSERT INTO declarations
VALUES
    (91,  1,   1, 1, 1, 1, 1, 1),
    (92,  2,   1, 1, 1, 1, 1, 1),
    (93,  3,   1, 1, 1, 1, 1, 1),
    (94,  4,   1, 1, 1, 1, 1, 1),
    (95,  5,   1, 1, 1, 1, 1, 1),
    (86,  6,   2, 1, 2, 2, 2, 2),
    (96,  6,   1, 1, 1, 1, 1, 1),
    (97,  7,   1, 1, 1, 1, 1, 1),
    (942, 42,  1, 1, 1, 1, 1, 1),
    (201, 101, 3, 1, 1, 1, 1, 1),
    (202, 102, 3, 1, 1, 1, 1, 1),
    (242, 142, 3, 1, 1, 1, 1, 1),
    (301, 301, 4, 1, 1, 1, 1, 1);

INSERT INTO
    symbol_refs(from_decl, to_symbol, from_line, from_col_start, from_col_end)
VALUES
    (91,  2,   1, 16, 16),
    (92,  4,   1,  1,  1),
    (93,  2,   1, 22, 22),
    (94,  5,   1,  1,  1),
    (94,  6,   1,  1,  1),
    (95,  6,   1,  1,  1),
    (97,  5,   1,  1,  1),
    (942, 1,   1,  1,  1),
    (942, 2,   1,  1,  1),
    (201, 102, 1,  5,  5),
    (242, 101, 1, 10, 10);

-- Module "test" has the same layout as test_input_b.
-- Module "other" mirrors a subset of the data to exercise module filtering.
-- Module "project_only" belongs to a separate project to exercise project filtering.
