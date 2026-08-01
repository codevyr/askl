# Askl cookbook

Definitions and location
- Define a symbol:            `"vfs_read"`
- Only functions named X:     `func("vfs_read")`
- Several names at once (OR): `"vfs_read" "vfs_write"`
- Functions in a file:        `file("/proj/fs/read_write.c") { func }`
- Files in a directory:       `dir("/proj/fs") { file }`

Call graph
- What X calls (callees):     `"vfs_read" { }`
- Who calls X (callers):      `{ "vfs_read" }`
- Typed, less noisy callers:  `func { "vfs_read" }`
- Two levels of callees:      `"vfs_read" { { } }`
- Transitive callees:         `"vfs_read" unnest { func }`

Containment
- Functions inside a macro:   `macro("LOG") { func }`
- Fields of a struct:         `type("file_operations") has { field }`
- Dispatch through a field:   `{ field("file_operations.read") }`

Full-text
- Find a literal:             `search("mmap_lock")`
- Whole word, scoped:         `project("linux") search("EXPORT_SYMBOL", whole_word="true")`
- Children of each hit:       `search("kmalloc") { }`

Scope and hygiene
- Restrict to a project:      `project("linux") "main" { }`
- Exclude test/helpers:       `ignore("test") "main" { }`
- Read raw lines:             use the `askl_read` tool with a `file` and line range.
