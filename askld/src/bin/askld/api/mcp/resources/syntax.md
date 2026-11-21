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
  only for non-symbol text. `>=3` chars, smart-case, options `whole_word=true`,
  `case="sensitive"|"insensitive"`, `limit=500`.
- Several selectors in one statement are **ORed** (union): `"a" "b"`,
  `func("a") func("b")` or `func("a") data("b")` selects symbols matching
  *either* — a named selector carries its own type. A bare type word beside
  them filters all of them (`func "a" "b"`), so two type words side by side
  (`macro func`, `data func("a")`) are an error: write `macro or func`.
  `"a" or "b"` is
  the explicit spelling (one no-match warning for the group — see Boolean
  operators). Inside a scope this is exactly the contrast with `;` —
  `X { A B }` disjoins, `X { A ; B }` conjoins. See Scopes.

## Scopes — relationships
- `"x" { }`  callees: what `x` calls/references (a bare scope accepts references
  or containment; two functions can only reference each other).
- `{ "x" }`  callers: what calls `x`.
- `"a" { "b" { "c" } }`  a calls b, b calls c.
- `has { }`  containment (the indexer's tree, not byte ranges) instead of references.
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
- `X { }` lists what is inside `X` at **any depth**, except **folded** symbols:
  ones nobody wrote down in normal code, i.e. what macros generated for
  themselves (`__compiletime_assert_N`, an unused `name##_show`).  Folding only
  hides — `X { }` still lists what sits *inside* a folded declaration.
- `X { unnest }` lists the folded symbols too.  `X { nested }` lists exactly
  one level down.  The mode goes **inside** the braces, on the statement whose
  reach it bounds (upward: `unnest { "x" }`), and does not inherit.
- Naming a symbol always finds it, folded or not: `X { "__compiletime_assert_7" }`.
- A listing that passed over folded symbols says so in `warnings`: "`{ }` leaves
  out 1 folded (macro-generated) symbol: `__se_sys_read`. Add `unnest` …".
  Heed it in a call chain: a generated function nobody names breaks the chain
  there (`"__x64_sys_read" { }` lists no callee), and `unnest` on that
  statement shows the hop.  A listing a limit cut says "incomplete" instead.
- **Macros (C).** Using a macro is an edge from whoever typed the use:
  `f -> BUILD_BUG_ON`. What is typed in a macro's arguments belongs to that
  same function, not to a declaration the macro generated
  (`BUILD_BUG_ON(sizeof(struct x))` in `f` gives `f -> x`). A helper handed to
  a generating macro has an edge from **each** generated function that uses it:
  `KHASH_INIT(x, int, my_hash)` gives `kh_get_x -> my_hash` and
  `kh_put_x -> my_hash`, while `KHASH_INIT` itself is an edge from the file.
- **`#include` inside a function or an initializer (C).** A dispatcher or
  table built from a header of `__SYSCALL(0, sys_read)` lines refers to what
  those lines name: `"x64_sys_call" { func }` lists the system calls.  The
  header refers to them as well.
- **A macro leads to what its invocations declared.** `DEFINE_MUTEX(lock)`
  gives `file -> DEFINE_MUTEX -> lock`, not `lock -> DEFINE_MUTEX`; a function
  written `PROFILE_FUNC(int, send, x) { … }` is reached from `PROFILE_FUNC`
  and does not lead to it.  `macro("DEFINE_MUTEX") { }` lists every mutex
  declared with it; `{ "lock" }` shows `DEFINE_MUTEX` beside the users.  A
  macro that only redeclares (`EXPORT_SYMBOL(foo)`) has no edge to `foo`.  A
  macro is ONE node: `A -> MACRO -> d` does not mean `A`'s invocation
  declared `d`, so do not read a path through a macro as a call chain.
- "too many results in `<statement>`: this part of the graph is incomplete"
  means a limit cut that statement's answer.  Folded symbols a listing leaves
  out never cause it.  Counts then read "N of at least M" (M is how far the
  reading got, not a total) and JSON carries `"incomplete": true`;
  `"truncated"` alone means everything was found and `limits(symbols=)`
  shows more of it.

## Filters — constrain, add nothing
- A **bare** type verb is a filter: `func`, `type`, `data`, `macro`, `field`,
  `mod`, `file`, `dir`, and `any`. **It filters the statement it is written
  in, and no other.** `func { "x" }` is the functions that use `x`, whatever
  `x` is (data, a macro, a type); `"x" { func { } }` leaves the innermost
  scope open to every type. To carry a type into every scope below, ask:
  `data(inherit=true) { { "x" } }`.
- **`any` is the type verb that constrains no type.** With a name it matches
  that name at every type: `any("ioctl")` finds the function, the macro and
  the struct. Bare, it drops a type carried down by `inherit=true`.
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
- A filter group inherits into `{ }` as a unit, when every positive member
  would on its own: `project("a") or project("b")` does, `func or method`
  does not (a type word stays in its statement). An inherited group is
  **replaced wholesale** by a child writing the same dimension.
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

## Reading an edge
- A pair is drawn once, with EVERY relationship it has, one mark each:
  `▸` contains; `→` refers to (a token of the source names the target); `↦`
  declares (the source's invocation declared the target: `DEFINE_MUTEX ↦
  lock`); `⇢` holds (a slot and a function stored in it, an interface method
  and an implementation).  `▸,→ bt_inb` under a file: the file contains
  `bt_inb` and something in it refers to `bt_inb`.  The report does not pick
  one; its legend names the marks it used.
- Words after the type say what the text does to the target, where the index
  states it: `→ handler  (func, takes)`.  `calls`: it is the callee of a
  call.  `writes`: it is assigned or initialised.  `takes`: a function's
  address is passed, stored or returned.  `reads`: the value of data or of a
  field is used.  JSON carries the same words as `kinds` on an edge.
- **No word means nothing is stated**, not "not called": an older index and a
  name in a `#define` body say nothing about what they do.

## Recipes — the short list (more in the `askl://cookbook` resource)
- Find a symbol by fuzzy name:   `g"*color*"`  (prefer over `search()` for names)
- Who calls X:                   `func { "vfs_read" }`
- What X calls:                  `"vfs_read" { func }`
- Functions in a file:           `file("/proj/fs/read_write.c") { func }`
- Implementations of an ops field: `"reg_mr" { func }`  — a struct field's
  children are the functions assigned to it in initializers, so this one query
  lists every `.reg_mr = xxx_reg_mr` implementation; don't grep for `.field =`.
- Everything inside X, generated symbols too: `mod("amdgpu") { unnest func }`
- Full-text (non-symbol text):   `project("linux") search("EXPORT_SYMBOL", whole_word=true)`

To read a listed symbol's **body**, re-run the query with `projection="body"`
on `askl_run` — exact definition, no line-range guessing.  Use `askl_read`
(file + line range) only for non-symbol regions.
