# Askl query syntax

Askl is a pattern-matching language over an indexed code graph. A query is one or
more **statements**. A statement has selector/filter verbs and an optional `{ }`
**scope** that expresses a relationship to nested statements. Newlines or `;`
separate statements; a `{` must be on the same line as its verb to attach.

## Selectors — add symbols to the result
- `"name"` — exact, **case-sensitive**. With no separator it matches the **last
  path component** (leaf): `"main"` matches `main`, not `domain` or `main_helper`.
  With `.` `/` `:` it is a token pattern over the full symbol path
  (`"http.Handler"` needs both tokens in order).
- `g"pat*"` — **glob** (wildcards are opt-in; a `*` in a plain `"..."` is literal).
  `*` = any run. Anchored to the whole leaf, so use `*x*` for "contains".
  Smart-case: all-lowercase = case-insensitive, any uppercase = case-sensitive.
  Needs a run of >=3 literal characters to use the index.
- Typed selectors: `func("n")`, `type("n")`, `data("n")`, `macro("n")`,
  `field("n")`/`method("n")`, `mod("n")`, `file("n")`, `dir("n")`.
- `search("literal")` — full-text over raw source bytes; one result symbol per
  match. **Literal only — no regex** (`search("a|b")` matches the text `a|b`; to OR
  literals, `search("a") search("b")` or `search("a"); search("b")` — both union). To find a
  **symbol by name, prefer a glob `g"*name*"`** (indexed, cheaper) — use `search()`
  only for non-symbol text. `>=3` chars, smart-case, options `whole_word="true"`,
  `case="sensitive"|"insensitive"`, `limit=500`.
- Several selectors in one statement are **ORed** (union): `"a" "b"` (or
  `func("a") func("b")`) selects symbols matching *either*. Inside a scope this
  is exactly the contrast with `;` — `X { A B }` disjoins, `X { A ; B }`
  conjoins. See Scopes.

## Scopes — relationships
- `"x" { }`  callees: what `x` calls/references (references are the default).
- `{ "x" }`  callers: what calls `x`.
- `"a" { "b" { "c" } }`  a calls b, b calls c.
- `has { }`  containment (by source byte-range) instead of references.
- **`;` inside a scope conjoins — `A B` disjoins.** `X { A ; B }` are two
  *statements* in one scope and keep `X` only where it relates to *both*:
  `dir("/proj") has { mod ; file }` lists directories holding a module **and**
  a file. `X { A B }` is one statement with two selectors and keeps `X` if it
  relates to *either*: `dir("/proj") has { mod file }` lists directories
  holding a module **or** a file. A child that matches nothing empties the
  whole conjunction, so `"x" { "b" ; "typo" }` returns nothing.
  (At the **top level**, `;` just separates statements and their results are
  unioned into one answer — it only conjoins inside `{ }`.)
- Container selectors `mod`/`file`/`dir` imply refs+has for children, so
  `file("/proj/x.c") { func }` lists functions in the file — no explicit `has`.
- `unnest { }`  transitive (all levels), not just direct. Does not inherit.

## Filters — constrain, add nothing
- A **bare** type verb is a filter that **inherits** to all descendants:
  `func`, `type`, `data`, `macro`, `field`, `mod`, `file`, `dir`. Use `any` in a
  child scope to drop an inherited type filter.
- **Duality:** `func("x")` (with a name) is a *selector* that queries; bare `func`
  is a *filter* that constrains. This is the most common point of confusion.
- `project("name")` restrict to one project (list names with the `askl_projects`
  tool). `ignore("pat")` exclude matches.
- **Scoping to a project is cheaper as an argument than as a verb:** pass
  `projects: ["linux"]` to `askl_run` and the other projects stop existing for
  that call, instead of being filtered out of the results afterwards. Use
  `project("name")` inside the query when a single query must span projects and
  name one of them.

## Paths
- `file()`/`dir()` arguments starting with `/` are **exact** and paths are
  **project-prefixed** (the prefix is the project's root — see `askl_projects`):
  `file("/linux/fs/read_write.c")`, not `"fs/read_write.c"`.
- A **partial** path (has `/` but no leading `/`, e.g. `file("fs/read_write.c")`)
  is a leaf-anchored pattern and can match **several** files (here also
  `.../ecryptfs/read_write.c`). Use the exact `/project/…` path to pin one.
- A simple name (no `/` `:`) is a leaf match: `file("read_write.c")`.
- `dir("kueue", match="contains")` matches "kueue" anywhere in the path.

## Rules
- Every statement must contain at least one selector at some nesting level.
- A scope only yields results if the relationship actually exists in the code.

## Recipes — the short list (more in the `askl://cookbook` resource)
- Find a symbol by fuzzy name:   `g"*color*"`  (prefer over `search()` for names)
- Who calls X:                   `func { "vfs_read" }`
- What X calls:                  `"vfs_read" { func }`
- Functions in a file:           `file("/proj/fs/read_write.c") { func }`
- Implementations of an ops field: `"reg_mr" { func }`  — a struct field's
  children are the functions assigned to it in initializers, so this one query
  lists every `.reg_mr = xxx_reg_mr` implementation; don't grep for `.field =`.
- Transitive callees:            `"vfs_read" unnest { func }`
- Full-text (non-symbol text):   `project("linux") search("EXPORT_SYMBOL", whole_word="true")`

To read a listed symbol's **body**, re-run the query with `projection="body"`
on `askl_run` — exact definition, no line-range guessing.  Use `askl_read`
(file + line range) only for non-symbol regions.
