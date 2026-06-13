# OpenGQL → SurrealQL AST lowering (v1, read-only)

This is the normative design for `surrealdb/core/src/opengql/lower/`. It maps the
GQL AST (`opengql::ast`) onto the SurrealQL surface AST (`crate::sql`), which the
engine executes via `Datastore::process` (`sql::Ast → expr::LogicalPlan` is the
mechanical `From` in `sql/ast.rs`). **No SurrealQL text is ever generated or
re-parsed** — the lowering constructs `sql::*` values directly; the SurrealQL
shown below is the `ToSql` rendering of those values, used for snapshot tests and
human verification.

Every engine behavior this design relies on is pinned by
`language-tests/tests/opengql/lowering_substrate.surql` (referenced below as
E1–E8), which passes under all planner strategies (`compute-only`, `all-ro`,
`best-effort-ro`). If that test ever breaks, this design must be revisited.

## 1. Model mapping

| GQL | SurrealDB |
|---|---|
| node label `(:person)` | table `person` |
| edge type `[:knows]` | RELATE edge table `knows` (records with `in`/`out` RecordIds) |
| property | record field |
| node/edge identity | the record's `id` (returned objects keep `id`, edges keep `in`/`out`) |
| parameter `$x` | SurrealQL param `$x` (arrives via the `vars` argument) |

## 2. The binding model

GQL returns **one row per binding** of the pattern variables. The IR facts that
force the shape (all pinned):

- A graph `Lookup` with an inline projection (`->(SELECT * FROM knows WHERE …)`)
  yields an **array of full edge objects**, with the lookup `cond` filtering
  per-edge (E1).
- `SPLIT` unnests one array field into rows, but an **empty array passes the row
  through** under the streaming engine, and splitting two correlated fields
  produces a cross product — so: exactly **one split field per hop**, and an
  explicit `WHERE __m != []` guard for inner-join semantics (E2).
- Within one SELECT, WHERE runs **before** SPLIT, and the legacy engine splits on
  *output* docs — so the unnest needs its own `SELECT *` layer, and per-binding
  predicates need a third, outer layer.
- `$parent` inside a lookup `cond` refers to the enclosing row (E4); `$this` is
  the current row (E1).
- The far node is **derived from the edge** (`__m.out` / `__m.in`); RecordId
  field access auto-fetches (`__m.out.name`), and `.*` fetches the full record
  (`__m.out.*`) (E3). This makes (edge, node) pairing structural — nothing to
  join.

### 2.1 Shapes

**No edge step** — `MATCH (n:person) …` collapses to a single SELECT
(no L2/L3): `n` → `$this`, `n.x` → `x`.

**One edge step** — `MATCH (a:person)-[k:knows]->(b:person) WHERE … RETURN …`
lowers to three nested SELECTs:

```sql
-- L1 "bind": one row per anchor; __m = matching edges as full edge objects
SELECT $this AS __a,
       ->(SELECT * FROM knows WHERE record::tb(out) = 'person' AND <edge-scope preds>) AS __m
FROM person WHERE <anchor-scope preds>
-- L2 "unnest": one row per (anchor, edge); guard enforces inner-join in BOTH engines
SELECT * FROM (L1) WHERE __m != [] SPLIT __m
-- L3 "return": residual predicates, projection, DISTINCT, ORDER, SKIP, LIMIT
SELECT <projections> FROM (L2) [WHERE <residual>] [GROUP BY <aliases>]
       [ORDER BY …] [LIMIT l] [START s]
```

- Direction `->` = `Lookup{kind: Graph(Dir::Out)}`, far end `out`;
  `<-` = `Graph(Dir::In)`, far end `in`.
- The b-side label filter is `record::tb(out) = '<label>'` (resp. `in`); omit it
  when the non-anchor node has no label.
- The anchor is the **leftmost node** and must be labeled (its table is the
  `FROM`). Otherwise reject.
- L2 must project `*` (legacy splits output docs) and must carry the
  `__m != []` guard (streaming passes empty arrays through).
- Multi-hop (>1 step) is rejected in v1; the scaffold extends with one
  `__m<i>` + split layer per hop.

### 2.2 Variable addressing

Post-split scope (L3 projections, residual predicates, ORDER BY):

| GQL | idiom |
|---|---|
| `a` | `__a` |
| `a.x` | `__a.x` |
| `k` | `__m` |
| `k.x` | `__m.x` |
| `b` | `__m.out.*` (or `__m.in.*` for `<-`) |
| `b.x` | `__m.out.x` |

Edge scope (predicates pushed into the L1 lookup `cond`):

| GQL | idiom |
|---|---|
| `k.x` | `x` |
| `b.x` | `out.x` (resp. `in.x`) |
| `a.x` | `$parent.x` |

Anchor scope (L1 `cond`): `a.x` → `x`.

Internal binding fields use the reserved `__` prefix; GQL variables, aliases and
parameters starting with `__` are rejected. Parameters named
`this, self, parent, value, before, after, event, auth, session, token, access`
are rejected (engine-reserved).

## 3. Predicate placement

1. Merge: explicit pattern WHERE ∧ node/edge inline WHEREs ∧ property-map
   equalities (a property map `{city: 'London'}` is sugar for `n.city = 'London'`
   conjuncts attached to its element).
2. Normalize to **negation normal form** (push NOT through AND/OR and into
   comparisons; needed for the 3VL guards below). Never distribute ORs.
3. Split top-level ANDs into conjuncts and classify each by the variables it
   references:
   - **⊆ {anchor} (or none)** → L1 `cond` (anchor scope rewrite).
   - **references k or b** (incl. mixed with a), with every non-anchor
     reference a property access → the lookup `cond` (edge-scope rewrite with
     `$parent`). This is correct, not just an optimization: all of {a,k,b}
     are visible there.
   - **fallback** (not expressible in edge scope: a *bare* `k`/`b` reference
     — the lookup scope cannot address the full record — or any non-anchor
     reference on a variable-length hop) → L3 `cond` (post-split rewrite).
     Always semantically valid, just scans more.
4. The L2 `__m != []` guard is structural, independent of user predicates.

Conjunct folding preserves the user's boolean spine: an n-conjunct chain
lowers to an n-deep `sql::Expr` AND spine, the same shape syn-parsed
SurrealQL produces for the equivalent WHERE. Operator-spine depth is bounded
at parse on both front-ends by `CommonConfig::max_expression_parsing_depth`
(default 128): syn's `ParserSettings::expr_recursion_limit` and the
mirroring `GqlParserSettings::expr_recursion_limit` charge a shared budget
per nesting level *and* per operator appended to a flat spine, so the
recursive walks downstream (drop, `ToSql`, the `sql::Ast →
expr::LogicalPlan` conversion) stay well within the machine stack. The §4
guard expansion multiplies a comparison leaf into at most five conjuncts —
a bounded constant factor (≤ ~640 lowered levels at the default limit,
~20× under the observed overflow threshold, and covered by the deep-chain
lowering tests).

## 4. Three-valued logic (the guard rules)

SurrealQL comparisons are two-valued over a total order (`NONE`/`NULL` sort below
numbers; `NULL = NULL` is true — E8c), while GQL comparisons with null are
UNKNOWN and WHERE keeps only TRUE. After NNF, lower each leaf:

- Ordering comparison `x OP y` (`<` `<=` `>` `>=`):
  `x != NONE AND x != NULL AND y != NONE AND y != NULL AND (x OP y)` —
  guarding only operands that can be null/missing (property accesses, params;
  never literals). Pinned by E8a/E8b.
- Equality `=`: guard only when **both** sides are nullable (covers the
  `NULL = NULL → true` delta); `x = <literal>` needs no guard.
- Inequality `<>`: guard when **either** side is nullable — `NULL != 'A'` is
  true in SurrealQL but UNKNOWN (excluded) in GQL, so a one-sided null must
  exclude the row.
- These guards apply **in every scope, including lookup conds** — E4 pins the
  hazard (`out.age < $parent.age` with a missing `out.age` is TRUE unguarded).
- `NOT b.flag` (bare nullable boolean) → `b.flag = false`
  (UNKNOWN→excluded, FALSE→kept — matches GQL in WHERE position).
- `x IS NULL` → `(x = NULL OR x = NONE)`; `IS NOT NULL` → negation. GQL cannot
  observe SurrealDB's NONE-vs-NULL distinction in v1 (document in user docs).
- `x IS TRUE|FALSE|UNKNOWN [NOT]` → equality against `true`/`false`, with
  `IS UNKNOWN` → `(x = NULL OR x = NONE)`; `IS NOT …` negates the whole test.
- `XOR`: lower as boolean inequality of guarded operands, or reject in v1 if not
  cleanly expressible — implementer's choice, but the choice must be a snapshot
  test either way.

## 5. RETURN, DISTINCT, ORDER, SKIP, LIMIT

- Projection: `sql` `Fields::Select` with one `Field::Single` per item; **always
  emit an explicit alias**, as a **single** `Part::Field` whose name may contain
  dots (E5/E6 pin dotted aliases working in ORDER/GROUP BY). Explicit `AS x`
  wins; unaliased items use the **verbatim source text** of the expression
  (`ReturnItem.text`) as the column name.
- Duplicate column names → reject ("duplicate column name; use AS").
- `RETURN *` → all named pattern variables, alphabetical order, each as a column
  named by the variable.
- `RETURN DISTINCT` → `GROUP BY` all projected aliases in L3 (E6).
- ORDER BY: `Order { value: <alias idiom>, direction }` when the sort key
  matches a RETURN item — by dotted name or by lowering to the same expression
  (alias resolution is engine-side). Any other sort key is **rejected**: the
  legacy engine sorts the projected output rows (a non-column key silently
  no-op sorts) while the streaming engine resolves source fields, so only
  column-matching keys behave identically under every planner strategy — the
  same invariant `syn` enforces for plain SELECTs ("Missing order idiom").
  `NULLS FIRST|LAST` → reject in v1 (no engine mapping).
- `SKIP`/`OFFSET` → `START`; `LIMIT` → `LIMIT`; both accept integer literals or
  `$param`.
- List literals `[…]` and record literals `{…}` lower to SurrealQL
  array/object literals, in any value position (e.g.
  `RETURN [n.age, 1] AS lst, {a: n.age} AS mp`).
- Aggregates in RETURN (`count`, `sum`, …) → reject in v1 ("aggregates not
  supported yet"). `FunctionCall.star`/`quantifier` (e.g. `count(*)`,
  `count(DISTINCT x)`) get the same targeted rejection.
- Function whitelist v1: empty (every function call is rejected with "function
  X is not supported yet") — extending it is a deliberate, tested act.

## 6. Variable-length edges

`-[:knows]->{1,3}` (single edge step, **no edge variable, no edge predicate,
min = 1**) lowers to a recursion idiom bound as the hop field:

```sql
SELECT $this AS __a, id.{1..3+collect}(->knows->person) AS __m FROM person
```

i.e. `Part::Recurse(Recurse::Range(Some(1), Some(3)), Some(idiom![->knows->person]),
Some(RecurseInstruction::Collect{inclusive: false}))` (E7). The elements of `__m`
are RecordIds, so `b` → `__m.*` and `b.x` → `__m.x` in post-split scope.
`{1}` → `Recurse::Fixed(1)`.

**Documented deviation**: `Collect` deduplicates, so v1 semantics are "distinct
reachable nodes", not GQL's one-row-per-path. `*`/`+`/`{0,m}`/`?`, quantifiers
with an edge variable or predicate, and quantified groups are rejected.

**Minimum exactly one**: quantifiers with min ≥ 2 (`{2}`, `{2,4}`) are
rejected ("not supported yet"). The streaming engine's collect BFS inserts a
node into its dedup set at first discovery *before* the min-depth collect
check, so a node first reached below the minimum is never emitted even when
it is also reachable within [min, max] — and the legacy engine disagrees, so
the behavior diverges across planner strategies and cannot be pinned by a
substrate test. Lift the restriction only once that engine issue is fixed
and the behavior is pinned. With min = 1 the only depth-0 discovery is the
anchor itself, which `Collect{inclusive: false}` does not seed into the
dedup set, so nothing can be dropped.

## 7. Rejection list (lowering errors; parse already rejected its own share)

Each produces a `SyntaxError` with the construct's span and a "not supported
yet"-style actionable message:

multiple MATCH clauses · `OPTIONAL MATCH` · comma-separated patterns ·
path variables `p =` · >1 edge step · undirected/mixed edge directions (only
`Left`/`Right` lower; `Undirected`, `LeftOrUndirected`, `UndirectedOrRight`,
`LeftOrRight`, `Any` reject) · label expressions beyond a single `Name` (`!`,
`&`, `|`, `%`) on nodes or edges · unlabeled anchor node · quantifier violations
(§6) · aggregates / any function call (§5) · `NULLS FIRST|LAST` · ORDER BY
expressions not matching a RETURN item (§5) · duplicate columns · `__`-prefixed
variables/aliases/params · engine-reserved param names · `XOR` (if the
implementer takes the reject option) · `UNKNOWN` literal in a non-boolean-test
position if not cleanly lowerable (`GqlLiteral` has no Unknown — truth-tests
carry it; nothing to do unless the parser surfaces it elsewhere).

## 8. Worked examples (snapshot-test anchors)

ToSql rendering may differ in whitespace/parens — assert against actual
`to_sql()` output once verified equivalent.

1. `MATCH (n:person) RETURN n`
   → `SELECT $this AS n FROM person`
2. `MATCH (n:person) WHERE n.age > 18 RETURN n.name AS name ORDER BY name SKIP 5 LIMIT 10`
   → `SELECT name AS name FROM person WHERE age != NONE AND age != NULL AND age > 18 ORDER BY name LIMIT 10 START 5`
3. `MATCH (a:person)-[:knows]->(b:person) RETURN a.name, b.name`
   → ```SELECT `a.name` (= __a.name), `b.name` (= __m.out.name) FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM knows WHERE record::tb(out) = 'person') AS __m FROM person) WHERE __m != [] SPLIT __m)```
4. `MATCH (a:person)-[k:knows]->(b:person) WHERE k.since > 2020 RETURN a, k, b`
   → L1 lookup cond: `(since != NONE AND since != NULL AND since > 2020) AND record::tb(out) = 'person'`; L3: `SELECT __a AS a, __m AS k, __m.out.* AS b FROM (…)`
5. `MATCH (n:person {city: 'London'}) RETURN n`
   → `SELECT $this AS n FROM person WHERE city = 'London'` (equality vs non-null literal: no guard)
6. `MATCH (a:person)-[:knows]->{1,3}(b:person) RETURN b`
   → §6 recursion shape, L3 `SELECT __m.* AS b FROM (…)`
7. `MATCH (a:person)-[k:knows]->(b:person) RETURN DISTINCT b.name`
   → L3 `SELECT __m.out.name AS ⟨b.name⟩ FROM (…) GROUP BY ⟨b.name⟩`

## 9. Unused clause fields

Fill `sql::SelectStatement` fields not driven by the GQL query with the same
defaults the SurrealQL parser produces for an equivalent plain SELECT (check
`syn`'s SELECT statement parser and the GraphQL precedent `gql/tables.rs` —
expr-layer analog). Snapshot tests make any mistake visible immediately.

## 10. Result shape caveat (document, don't fight)

Rows are `Value::Object` keyed by column name; SurrealDB objects are key-ordered,
so the RETURN-list column order is not preserved in the value. The GQL AST keeps
the ordered column list (`ReturnClause.items`) for a future row-set wire format.
