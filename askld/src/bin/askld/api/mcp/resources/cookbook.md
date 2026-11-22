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
- A call across an ops table: `"vfs_read" { field { func } }`  (`vfs_read` calls the slot `file_operations.read`; the functions stored in it are one hop further. Name the field or a function to narrow: `"vfs_read" { field("file_operations.read") { g"bt_bmc*" } }`. `"vfs_read" { func { func } }` skips the slot — a field is not a function — and drops every function behind it)

Containment and macros
- Functions a macro's body calls: `macro("LOG") { func }`
- What a macro's invocations declared: `macro("DEFINE_MUTEX") { data }`, `macro("UCS_PROFILE_FUNC") { func }`  (add `unnest` for the ones nobody names)
- Macros that declare or use a symbol: `macro { "cgroup_mutex" }`  (the word `macro` is about the users only; the name matches at every type)
- Everything in a module, incl. what macros generated: `mod("amdgpu") { unnest func }`  (the default leaves out symbols nobody wrote down, e.g. `__compiletime_assert_N`)
- Fields of a struct:         `type("file_operations") has { field }`
- Only what a file defines:   `file("/proj/fs/read_write.c") has { func }`  (bare `{ func }` also lists the functions it refers to)
- Files that define X:        `dir("/proj/fs") has inherit(relationship) { file { func("init_module") } }`  (`inherit(relationship)` carries `has` into the inner scope; without it the inner scope is bare and files that only mention X are listed too)
- Who uses a field:           `{ field("file_operations.read") }`  (everything that uses the field — functions that call through it, test it or assign it — and what contains it; `func { … }` keeps the functions)

By relationship — the query for today's indices first, then the word that asks for exactly it
(No published index states roles or has `declares` / `holds` rows yet. On today's indices the second form finds nothing and `warnings` says so — "the `linux` index states no roles; `calls` cannot match there. `refs { … }` lists its references" — which is NOT "nothing calls x". Use the first form and read the code.)
- Who calls through a slot, and who assigns it:
  today `func { field("platform_driver.probe") }`  (both together: on the kernel, `platform_probe` calls through it and six functions assign it; `projection="body"` shows which)
  words `func calls { field("platform_driver.probe") }` / `func writes { field("platform_driver.probe") }`
- What a macro declares, and what its body names:
  today `macro("DEFINE_MUTEX") { }`  (both together; a type word separates them where the types differ: `{ data }` the mutexes, `{ macro or type }` what the body names)
  words `macro("DEFINE_MUTEX") declares { data }` / `macro("DEFINE_MUTEX") refs { }`
- A call across an ops table:
  today `"vfs_read" { field { func } }`
  words `"vfs_read" calls { field holds { func } }`  (`calls` is literal: it reaches the slot; `holds` reaches what is stored in it)
- What a slot holds (implementations of an ops field):
  today `field("file_operations.read") { func }`
  words `field("file_operations.read") holds { func }`
- Either of two relationships: `"vfs_read" has or refs { }`  (words join with `or`; `and` / `not` between them are refused)
- A warning that gives a query ("follow it: `"via" calls { field holds { func } }`", "`calls { field holds { "my_read" } }` goes through the slot"): run that query.

Combining children — `;` is AND, a space is OR
- Relates to BOTH (AND):      `func { "vfs_read" ; "vfs_write" }`  (`;` inside `{ }` makes each child a separate statement and all must match — here: functions calling both)
- Relates to EITHER (OR):     `func { "vfs_read" "vfs_write" }`  (one statement, two selectors)
- Contains both kinds:        `dir("/proj") has { mod ; file }`  (vs `has { mod file }` = either)
- A child that matches nothing empties the AND: `"x" { "b" ; "typo" }` returns nothing.
- At the TOP level `;` is not AND — `"a" { } ; "b" { }` runs both statements and unions their results.

Full-text (raw source bytes — for text that is NOT a symbol name; to find a symbol prefer `g"*foo*"`)
- Find a literal:             `search("mmap_lock")`
- Whole word, scoped:         `project("linux") search("EXPORT_SYMBOL", whole_word=true)`
- Several literals (OR):      `search("foo") search("bar")`  (or as two top-level statements, `search("foo"); search("bar")` — both union; `search()` is literal, `search("a|b")` matches the text `a|b`, not a regex)
- Children of each hit:       `search("kmalloc") { }`
- Inside one function:        `func("vfs_read") { search("mmap_lock") }`
- Functions containing a literal: `func { search("mmap_lock") }`

Scope and hygiene
- Restrict to a project:      `project("linux") "main" { }`
- Exclude test/helpers:       `ignore("test") "main" { }`
- Read a symbol's body:       re-run the query with `projection="body"` on `askl_run` (e.g. `func("vfs_read")`) — the exact definition, no line-range guessing.
- Read raw lines:             use the `askl_read` tool with a `file` and line range — for non-symbol regions (headers, config, context around a hit) only.
