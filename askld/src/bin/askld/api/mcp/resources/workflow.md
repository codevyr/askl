# Exploring a codebase with askl

The query string *is* your accumulated context. Grow it step by step rather than
starting over — re-running an extended query is cheap (results are cached).

1. **Discover.** Call `askl_projects` to see indexed projects and their names.
   The project name is what `project("…")` scopes to.
2. **Root the query.** Find an entry point:
   - a name you know: `"vfs_read"`
   - by text: `search("EXPORT_SYMBOL_GPL")`
   - by location: `file("/proj/path.c") { func }`
3. **Pick a direction.**
   - callees (what it calls): `"vfs_read" { }`
   - callers (who calls it): `{ "vfs_read" }`
   - go deeper by nesting: `"vfs_read" { func { } }`
   - a bare `{ }` follows every relationship; a word narrows it: `has`
     (containment), `refs` (references). `calls`, `writes`, `takes`,
     `declares` and `holds` ask for more than today's indices state — see
     the last paragraph.
4. **Narrow the noise.** Add filters as you learn: `func` to keep functions,
   `project("…")` to stay in one tree, `ignore("…")` to drop helpers. A typed
   caller scope `func { "x" }` is far less noisy than a bare `{ "x" }`.
5. **Deepen the projection** only where you need it:
   - `projection: "names"` — just identifiers (widest overview)
   - `projection: "signature"` (default) — one line per symbol
   - `projection: "body"` — full definition + doc comment, for the few symbols
     you are actually reading.
6. **Read raw source** with `askl_read` for non-symbol context (headers, config,
   the lines around a `search()` hit). Symbol bodies come from
   `askl_run(projection: "body")`.

If a query returns nothing: **read `warnings` first**, then re-check the name
(case-sensitive), confirm the project with `askl_projects`, and re-read
`askl://syntax`. Errors almost always mean the query, not the tool — do not
fall back to grep.

A warning "the `linux` index states no roles; `calls` cannot match there"
(or "has no `declares` rows", "has no `holds` rows") means the empty answer
says nothing about the code: no published index records that yet. Drop the
word, or write `refs`, and run it again — never report "nothing calls x"
from it. A callee list that looks short for a dispatcher (`"vfs_read"
{ func }`) is missing the calls through struct fields: ask
`"vfs_read" { field { func } }`.
