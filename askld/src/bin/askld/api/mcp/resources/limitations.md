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
- **No published index states roles or has `declares` / `holds` rows yet.**
  `calls`, `writes`, `takes` / `reads`, `declares` and `holds` find nothing
  on today's projects and say so in `warnings` ("the `linux` index states no
  roles; `calls` cannot match there. `refs { … }` lists its references").
  That is not "nothing calls x": ask with a bare `{ }`, `refs` or `has`, and
  read the code for what a reference does.  Today's indices record what a
  macro declared and what a slot holds as ordinary references, so the bare
  scope and `refs` list them.
- **A word on an edge is what the text does, not what runs.** `calls` is a
  literal fact.  It does not follow a slot to what the slot holds:
  `vfs_read` calls `file_operations.read`, and the functions are one hop
  further (`⇢`): `"vfs_read" { field { func } }`.  A listing that leaves
  such a slot out (`"vfs_read" { func }`) says so in `warnings` and gives the
  query that follows it (`"vfs_read" calls { field holds { func } }`) only on
  an index that states roles and what slots hold; today it says nothing, so
  ask for the fields yourself.  `fn = s->cb; fn()` is a call through no
  callee the index can name, and `*p = x` a write to no field it can name.
  Invoking a function-like macro is not a call.  An edge without a word
  states nothing.
- **An unstated role is unknown, never "no".** Even where an index states
  roles, a name in a `#define` body or a macro argument states none; no role
  word matches it, and the answer counts what it passed over.
- **Relationship words join with `or` only.** `and` and `not` between them
  are refused: "functions that both call and assign this slot" is two
  queries.
- **One name is one symbol (C).** Every driver's `DEVICE_NAME` and every
  module's `init_module` is the same node. The report shows such a symbol at
  the definition nearest to whoever led to it and marks the choice:
  `(1 of 47 definitions)`. That is exact when a definition is in the file
  that refers to it and a guess otherwise, and a path through such a node
  (`a -> DEVICE_NAME -> b`) may join two unrelated files.
  What REFERS is one instance, though: `"main" { "print_version" }` keeps the
  `main` that makes the call, not every program's `main`, and a header
  declaration is not a caller.
- **`search()` is literal.** No regex — the query matches an exact byte sequence.
- **A `search()` match has one place.** Text inside a macro invocation that
  generated several declarations is reported under one of them. Scope by the
  function or file that contains the invocation, not by a generated symbol.
- **`search`/`loc`/layer literals cannot appear inside `or`/`and`/`not`;**
  union searches by juxtaposition (`search("a") search("b")`).
- **Result caps.** Results are capped per command (default 100 distinct
  symbols). The `limit` argument raises the default for the whole request;
  `limits(symbols=N, listing=M)` written on a command raises it for that
  command and its scope, `listing` bounding the instances and out-edges a node
  lists. When a result is truncated the report says so — narrow the query
  rather than raising the cap blindly.
