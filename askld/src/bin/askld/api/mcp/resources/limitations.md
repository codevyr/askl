# Askl limitations

- **Read-only.** Askl explores an existing index; it never modifies code.
- **Indexed languages.** Coverage is limited to what has been indexed (currently
  includes C; other languages depend on the available indexers). `askl_projects`
  shows what is present.
- **Symbol focus.** The graph is about functions/methods, types, data, macros,
  fields, files, directories, and modules. It is not a line-level diff or a
  runtime trace.
- **Static edges.** Call/reference edges come from static analysis. Indirect
  calls (function pointers, virtual dispatch) may be missing; `field`/`method`
  and `!forced` help model dispatch, but coverage is not guaranteed.
- **`search()` is literal.** No regex — the query matches an exact byte sequence.
- **Result caps.** Results are capped (default 100 distinct symbols; override with
  the `limit` argument). When a result is truncated the report says so — narrow
  the query rather than raising the cap blindly.
