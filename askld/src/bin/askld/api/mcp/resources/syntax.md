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
  `func("a") func("b")`) selects symbols matching *either*; `"a" or "b"` is
  the explicit spelling (one no-match warning for the group — see Boolean
  operators). Inside a scope this is exactly the contrast with `;` —
  `X { A B }` disjoins, `X { A ; B }` conjoins. See Scopes.

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
  `func`, `type`, `data`, `macro`, `field`, `mod`, `file`, `dir`, and `any`.
- **`any` is the type verb that constrains no type.** Bare `any` in a child
  scope drops an inherited type filter — `func("x") { any }` is everything `x`
  references, not just the functions. With a name it matches that name at every
  type: `any("ioctl")` finds the function, the macro and the struct.
- **Duality:** `func("x")` (with a name) is a *selector* that queries; bare `func`
  is a *filter* that constrains. This is the most common point of confusion.
  `any` follows the same rule: `any("x")` queries, bare `any` constrains.
- `project("name")` restrict to one project (list names with the `askl_projects`
  tool). `package("path")` keep only symbols under a package path.
  `ignore("pat")` exclude matches.
- **Scoping to a project is cheaper as an argument than as a verb:** pass
  `projects: ["linux"]` to `askl_run` and the other projects stop existing for
  that call, instead of being filtered out of the results afterwards. Use
  `project("name")` inside the query when a single query must span projects and
  name one of them.

## Boolean operators — or / and / not / ( )
- Precedence `not` > `and` > `or`; parentheses group. Operators bind tighter
  than whitespace: `func or method "foo"` is the type group plus a separate,
  juxtaposed selector `"foo"`.
- Anchors: `"open" or "close"` is ONE branch (one no-match warning);
  juxtaposed `"open" "close"` warns per name. Same rows either way.
- Filters: the operands of one `or` must share a dimension —
  `project("a") or project("b")`, `func or method`. Cross-dimension filters
  conjoin by **juxtaposition**, never by `and`/`or`; a cross-dimension
  disjunction is written as sibling statements.
- `not` excludes by negating the SAME predicate the positive verb uses:
  `not "test"` excludes symbols whose **leaf** is `test`, while
  `ignore("test")` is broader — it excludes anything carrying the path label
  `test` anywhere (so `ignore("foo")` drops `foo.bar`, `not "foo"` keeps it).
  They coincide on plain leaf names. `func and not g"test_*"` = functions
  except test_*. Exclusions accumulate and inherit.
- A filter group inherits into `{ }` as a unit and is **replaced wholesale**
  by a child writing the same dimension: `func or method { data "x" }` finds
  `x` as data.
- **One line per expression, unless a `(` is open**: an operator at the END
  of a line continues onto the next (`"a" or` ⏎ `"b"`), but a line STARTING
  with `or`/`and` is a new statement and errors. Newlines are free inside
  `( )`.
- Not allowed inside expressions: `search`/`loc`/`layer` (not a single
  predicate query), `has`/`refs`/labels (not predicates), bare `any` and
  `select` (constrain nothing), `!"..."` (forced). Union searches by
  juxtaposition instead: `search("a") search("b")`.

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
