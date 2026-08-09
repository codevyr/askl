# Askl cookbook

Definitions and location
- Define a symbol:            `"vfs_read"`
- Only functions named X:     `func("vfs_read")`
- Fuzzy symbol name (glob):   `g"*color*"`  (indexed, returns symbols — prefer this over `search()` to find a symbol by name)
- Several names at once (OR): `"vfs_read" "vfs_write"`
- Functions in a file:        `file("/proj/fs/read_write.c") { func }`
- Files in a directory:       `dir("/proj/fs") { file }`

Call graph
- What X calls (callees):     `"vfs_read" { }`
- Who calls X (callers):      `{ "vfs_read" }`
- Typed, less noisy callers:  `func { "vfs_read" }`
- Indirect calls (fn pointer):`func { method "color_adjust" { func } }`  (calls that dispatch through a struct fn-pointer field to its implementations; `method` = `field`, a type filter)
- Implementations of an ops field: `"reg_mr" { func }`  (a struct field's children are the functions assigned to it in initializers — ONE query lists every `.reg_mr = xxx_reg_mr` implementation across providers; no grep needed)
- Two levels of callees:      `"vfs_read" { { } }`
- Transitive callees:         `"vfs_read" unnest { func }`

Containment
- Functions inside a macro:   `macro("LOG") { func }`
- Fields of a struct:         `type("file_operations") has { field }`
- Dispatch through a field:   `{ field("file_operations.read") }`

Full-text (raw source bytes — for text that is NOT a symbol name; to find a symbol prefer `g"*foo*"`)
- Find a literal:             `search("mmap_lock")`
- Whole word, scoped:         `project("linux") search("EXPORT_SYMBOL", whole_word="true")`
- Several literals (OR):      `search("foo") search("bar")`  (or `;`-separated — both union; `search()` is literal, `search("a|b")` matches the text `a|b`, not a regex)
- Children of each hit:       `search("kmalloc") { }`

Scope and hygiene
- Restrict to a project:      `project("linux") "main" { }`
- Exclude test/helpers:       `ignore("test") "main" { }`
- Read a symbol's body:       re-run the query with `projection="body"` on `askl_run` (e.g. `func("vfs_read")`) — the exact definition, no line-range guessing.
- Read raw lines:             use the `askl_read` tool with a `file` and line range — for non-symbol regions (headers, config, context around a hit) only.
