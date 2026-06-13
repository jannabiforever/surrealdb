# ISO GQL — distilled reference for the v1 read-only subset

Derived from the vendored [`GQL.g4`](./GQL.g4) (opengql grammar 1.9.0, see
[`README.md`](./README.md) for provenance). Every section quotes the grammar
production(s) it is derived from; line numbers refer to `GQL.g4` as vendored.
The `// N.M <name>` comments in the grammar are the ISO/IEC 39075:2024 section
numbers.

Scope: the v1 read-only subset — `MATCH … WHERE … RETURN … ORDER BY …
OFFSET/SKIP … LIMIT …`. Constructs outside this subset are noted where they
share grammar real estate (so the parser can recognise and cleanly reject
them), but are not specified in full here.

> The whole grammar is declared `options { caseInsensitive = true; }` (line 3),
> so **all keywords are case-insensitive** (`match`, `Match`, `MATCH` are the
> same token). Identifier *text* is preserved as written; whether identifier
> *comparison* is case-sensitive is a semantic question outside the grammar
> (SurrealDB decision: treat identifiers case-sensitively, matching SurrealQL
> table/field semantics).

---

## (a) Lexical rules

### Whitespace

From `SP` / `WHITESPACE` (lines 3709–3744). Whitespace is one or more of:
space, `\t`, `\n`, `U+000B` (VT), `\f` (`U+000C`), `\r`, the C0 separators
`U+001C`–`U+001F` (FS/GS/RS/US), and the Unicode space set `U+00A0` (NBSP),
`U+1680`, `U+180E`, `U+2000`–`U+2006`, `U+2007` (figure space, no-break),
`U+2008`–`U+200A`, `U+2028`, `U+2029`, `U+202F` (narrow NBSP), `U+205F`,
`U+3000`. (The grammar lists `U+2007` and `U+202F` at the end of the rule,
after the `U+2000`–`U+2006`/`U+2008`–`U+200A` run — i.e. no-break spaces
*are* whitespace in GQL.)

### Comments

Three forms, all hidden-channel (lines 3746–3750):

| Token | Introducer | Terminator |
|---|---|---|
| `BRACKETED_COMMENT` | `/*` | first `*/` (non-greedy `.*?`) — **does NOT nest** |
| `SIMPLE_COMMENT_SOLIDUS` | `//` | end of line (`~[\r\n]*`) |
| `SIMPLE_COMMENT_MINUS` | `--` | end of line |

`--` being a line comment matters: openCypher's `()--()` undirected-edge
abbreviation is a **comment introducer** in GQL (see §f). An unterminated
`/*` is a lexical error.

`/** … */` test-headers therefore lex fine as bracketed comments (relevant to
`.gql` language-test files).

### Identifiers

From `identifier`, `regularIdentifier` (lines 2956–2966) and the lexer rules
`REGULAR_IDENTIFIER`, `DELIMITED_IDENTIFIER`,
`DOUBLE_QUOTED_CHARACTER_SEQUENCE`, `ACCENT_QUOTED_CHARACTER_SEQUENCE`
(lines 3586–3627, 3117–3155):

- **Regular**: `IDENTIFIER_START IDENTIFIER_EXTEND*` where start = Unicode
  `ID_Start` **or `Pc`** (connector punctuation — so `_foo` is valid) and
  extend = Unicode `ID_Continue`. No leading digits.
- **Regular identifiers also include the non-reserved words** —
  `regularIdentifier : REGULAR_IDENTIFIER | nonReservedWords` — so e.g. `node`,
  `type`, `first` are usable as variable/label/property names. Reserved words
  are not.
- **Delimited**: double-quoted `"…"` **or** accent-quoted (backtick) `` `…` ``.
  Both use the same escape regime as strings (below), including doubling
  (`""`, ` `` `) and the `@`-prefix no-escape mode.
- Caution: `"…"` is **both** a delimited identifier and a character string
  literal (same token, `DOUBLE_QUOTED_CHARACTER_SEQUENCE`); disambiguation is
  by parse position. The lexer must emit one token kind and let the parser
  decide.

### Character string literals

From `characterStringLiteral` (lines 2972–2975) and the
`SINGLE_QUOTED_/DOUBLE_QUOTED_CHARACTER_SEQUENCE` lexer rules (3117–3185):

- Quote chars: `'…'` and `"…"` (both are strings; `"…"` doubles as an
  identifier, above).
- An optional `@` prefix (`NO_ESCAPE`, line 3129) disables escape processing:
  `@'C:\dir'` is verbatim.
- Raw newlines (`\r`, `\n`) are **not** allowed inside any quoted sequence.
- Quote-doubling escapes the quote: `''`, `""`, ` `` `.
- Backslash escapes (`ESCAPED_CHARACTER`, lines 3157–3185): `\\`, `\'`, `\"`,
  `` \` ``, `\t`, `\b`, `\n`, `\r`, `\f`, `\uXXXX` (4 hex), `\UXXXXXX`
  (6 hex). The escape letters are case-sensitive (`caseInsensitive=false` on
  those fragments) — `\N` is not a newline escape.

### Numeric literals

From `unsignedNumericLiteral` … `unsignedInteger` (lines 2977–3002) and lexer
rules at 3192–3272:

- **Decimal integer**: `DIGIT (UNDERSCORE? DIGIT)*` — underscore digit
  separators allowed between digits (`1_000_000`), not leading/trailing/doubled.
- **Hex/octal/binary integers**: `0x` / `0o` / `0b` prefixes (prefix letters
  case-sensitive, lowercase only: `0X` is not a hex introducer), digits
  optionally `_`-separated: `0xdead_beef`, `0o777`, `0b1010`.
- **Common notation** (float): `123.`, `123.456`, `.456`.
- **Scientific**: mantissa (integer or common notation) `E` signed-integer
  exponent — `1.5e10`, `2E-3` (`E` case-insensitive here).
- **Suffixes**: exact-number suffix `M` (decimal/exact); approximate suffixes
  `F` and `D` (float/double). E.g. `1.5M`, `2.0F`, `3D`. Unsuffixed common
  notation is exact; unsuffixed scientific notation is approximate
  (`exactNumericLiteral` / `approximateNumericLiteral`, lines 2982–2995).
- No sign in the literal itself — `-3` is unary minus applied to `3`
  (`signedExprAlt`).

### Other literals (for completeness)

`generalLiteral` (line 2918): `BOOLEAN_LITERAL` is a **lexer token**
`'TRUE' | 'FALSE' | 'UNKNOWN'` (line 3111) — i.e. TRUE/FALSE/UNKNOWN behave as
reserved and are never identifiers. `nullLiteral : NULL_KW` (`NULL`). Also:
typed temporal literals (`DATE '…'`, `TIME '…'`, `DATETIME|TIMESTAMP '…'`,
`DURATION '…'` — keyword + string), `BYTE_STRING_LITERAL` (`X'48 65'`), list
literals `[…]`, record literals `{…}` (`RECORD?` + fields). v1 supports
boolean/null/string/numeric, and lowers list literals `[…]` and record
literals `{…}` to SurrealQL array/object literals (bare `{…}` only — no
`RECORD` keyword prefix); typed temporal literals parse-then-reject, and
byte strings are deferred.

### Parameters

From `GENERAL_PARAMETER_REFERENCE : DOLLAR_SIGN PARAMETER_NAME` and
`SUBSTITUTED_PARAMETER_REFERENCE : DOUBLE_DOLLAR_SIGN PARAMETER_NAME`
(lines 3604–3610), `PARAMETER_NAME : SEPARATED_IDENTIFIER` (3055):

- `$name` — general (value) parameter. **Note**: `PARAMETER_NAME` is a
  *separated* identifier — `DELIMITED_IDENTIFIER | EXTENDED_IDENTIFIER`
  (3586) where `EXTENDED_IDENTIFIER : IDENTIFIER_EXTEND+` — so the name may
  start with a digit or be quoted (`$"weird name"`, `` $`x` ``). v1: accept
  `ID_Continue+` and quoted forms, then validate against the engine-reserved
  names per the plan.
- `$$name` — substituted (pre-execution text substitution) parameter.
  **Reject in v1.**
- Parameters are valid where `dynamicParameterSpecification` appears — notably
  inside value expressions and as `LIMIT`/`OFFSET` counts
  (`nonNegativeIntegerSpecification : unsignedInteger |
  dynamicParameterSpecification`, line 2268).

---

## (b) Keywords

Three classes in the grammar (lexer section `// 21.3`, lines 3276–3584).
Reserved + prereserved words can **never** be identifiers; non-reserved words
can (`regularIdentifier : REGULAR_IDENTIFIER | nonReservedWords`).
TRUE/FALSE/UNKNOWN are additionally captured by `BOOLEAN_LITERAL`.

### Reserved words (lines 3277–3494, complete)

```
ABS ACOS ALL ALL_DIFFERENT AND ANY ARRAY AS ASC ASCENDING ASIN AT ATAN AVG
BIG BIGINT BINARY BOOL BOOLEAN BOTH BTRIM BY BYTE_LENGTH BYTES CALL
CARDINALITY CASE CAST CEIL CEILING CHAR CHAR_LENGTH CHARACTER_LENGTH
CHARACTERISTICS CLOSE COALESCE COLLECT_LIST COMMIT COPY COS COSH COT COUNT
CREATE CURRENT_DATE CURRENT_GRAPH CURRENT_PROPERTY_GRAPH CURRENT_SCHEMA
CURRENT_TIME CURRENT_TIMESTAMP DATE DATETIME DAY DEC DECIMAL DEGREES DELETE
DESC DESCENDING DETACH DISTINCT DOUBLE DROP DURATION DURATION_BETWEEN
ELEMENT_ID ELSE END EXCEPT EXISTS EXP FILTER FINISH FLOAT FLOAT16 FLOAT32
FLOAT64 FLOAT128 FLOAT256 FLOOR FOR FROM GROUP HAVING HOME_GRAPH
HOME_PROPERTY_GRAPH HOME_SCHEMA HOUR IF IN INSERT INT INTEGER INT8 INTEGER8
INT16 INTEGER16 INT32 INTEGER32 INT64 INTEGER64 INT128 INTEGER128 INT256
INTEGER256 INTERSECT INTERVAL IS LEADING LEFT LET LIKE LIMIT LIST LN LOCAL
LOCAL_DATETIME LOCAL_TIME LOCAL_TIMESTAMP LOG LOG10 LOWER LTRIM MATCH MAX MIN
MINUTE MOD MONTH NEXT NODETACH NORMALIZE NOT NOTHING NULL NULLS NULLIF
OCTET_LENGTH OF OFFSET OPTIONAL OR ORDER OTHERWISE PARAMETER PARAMETERS PATH
PATH_LENGTH PATHS PERCENTILE_CONT PERCENTILE_DISC POWER PRECISION
PROPERTY_EXISTS RADIANS REAL RECORD REMOVE REPLACE RESET RETURN RIGHT
ROLLBACK RTRIM SAME SCHEMA SECOND SELECT SESSION SESSION_USER SET SIGNED SIN
SINH SIZE SKIP SMALL SMALLINT SQRT START STDDEV_POP STDDEV_SAMP STRING SUM
TAN TANH THEN TIME TIMESTAMP TRAILING TRIM TYPED UBIGINT UINT UINT8 UINT16
UINT32 UINT64 UINT128 UINT256 UNION UNSIGNED UPPER USE USMALLINT VALUE
VARBINARY VARCHAR VARIABLE WHEN WHERE WITH XOR YEAR YIELD ZONED
ZONED_DATETIME ZONED_TIME
```

v1 actively uses: `MATCH OPTIONAL WHERE RETURN DISTINCT ALL ORDER BY ASC
ASCENDING DESC DESCENDING NULLS OFFSET SKIP LIMIT AND OR XOR NOT IS NULL AS`
(+ `TRUE FALSE UNKNOWN` via `BOOLEAN_LITERAL`). **Both `SKIP` and `OFFSET`
are valid** and synonymous — `offsetSynonym : OFFSET | SKIP_RESERVED_WORD`
(line 1374; `SKIP_RESERVED_WORD: 'SKIP'`, line 3452 — named that way only
because `skip` is an ANTLR-reserved symbol).

### Prereserved words (lines 3497–3535, complete — reserved for future ISO use, also not identifiers)

```
ABSTRACT AGGREGATE AGGREGATES ALTER CATALOG CLEAR CLONE CONSTRAINT
CURRENT_ROLE CURRENT_USER DATA DIRECTORY DRYRUN EXACT EXISTING FUNCTION
GQLSTATUS GRANT INSTANT INFINITY NUMBER NUMERIC ON OPEN PARTITION PROCEDURE
PRODUCT PROJECT QUERY RECORDS REFERENCE RENAME REVOKE SUBSTRING SYSTEM_USER
TEMPORAL UNIQUE UNIT VALUES
```

### Non-reserved words (lines 3538–3584 and `nonReservedWords`, lines 3061–3109, complete — valid as identifiers)

```
ACYCLIC BINDING BINDINGS CONNECTING DESTINATION DIFFERENT DIRECTED EDGE EDGES
ELEMENT ELEMENTS FIRST GRAPH GROUPS KEEP LABEL LABELED LABELS LAST NFC NFD
NFKC NFKD NO NODE NORMALIZED ONLY ORDINALITY PROPERTY READ RELATIONSHIP
RELATIONSHIPS REPEATABLE SHORTEST SIMPLE SOURCE TABLE TO TRAIL TRANSACTION
TYPE UNDIRECTED VERTEX WALK WITHOUT WRITE ZONE
```

Lexer guidance: keyword lookup must be case-insensitive (phf + UniCase as in
`syn`); classify into Reserved / Prereserved / NonReserved so the parser can
accept non-reserved words wherever `regularIdentifier` is required.

---

## (c) MATCH and patterns

### Statement context

A v1 query is an `ambientLinearQueryStatement` (line 554):
`simpleLinearQueryStatement? primitiveResultStatement` — i.e. zero or more
`matchStatement`s (via `simpleQueryStatement`, line 563; v1: exactly one)
followed by a result statement. The enclosing `compositeQueryExpression`
(line 504) adds `UNION | EXCEPT | INTERSECT | OTHERWISE` chaining —
**parse-and-reject in v1**. Likewise `focusedLinearQueryStatement`
(`USE graph …`), `selectStatement` (SQL-style SELECT, line 689), and the
other `primitiveQueryStatement`s (`LET`, `FOR`, `FILTER`, standalone
`orderByAndPageStatement`).

### Match statement (14.4, lines 578–599)

```
matchStatement         : simpleMatchStatement | optionalMatchStatement ;
simpleMatchStatement   : MATCH graphPatternBindingTable ;
optionalMatchStatement : OPTIONAL optionalOperand ;          // v1: reject
graphPatternBindingTable : graphPattern graphPatternYieldClause? ;  // YIELD: v1 reject
```

### Graph pattern (16.4, lines 803–848)

```
graphPattern   : matchMode? pathPatternList keepClause? graphPatternWhereClause? ;
pathPatternList: pathPattern (COMMA pathPattern)* ;          // v1: exactly one
pathPattern    : pathVariableDeclaration? pathPatternPrefix? pathPatternExpression ;
pathVariableDeclaration : pathVariable EQUALS_OPERATOR ;     // p = …   v1: reject
graphPatternWhereClause : WHERE searchCondition ;
```

Key fact: **the MATCH-level WHERE belongs to the graph pattern**, not to a
separate clause — `MATCH (a)-[k]->(b) WHERE …` parses inside `graphPattern`.
`matchMode` (`REPEATABLE ELEMENTS` / `DIFFERENT EDGES`, lines 807–828),
`keepClause`, and `pathPatternPrefix` (path modes `WALK|TRAIL|SIMPLE|ACYCLIC`,
path search `ALL|ANY|SHORTEST …`, lines 898–962) — all v1 reject.

### Path pattern expression (16.7, lines 966–991)

```
pathPatternExpression : pathTerm                                       #ppePathTerm
                      | pathTerm (MULTISET_ALTERNATION_OPERATOR pathTerm)+   // |+|  v1: reject
                      | pathTerm (VERTICAL_BAR pathTerm)+               // |    v1: reject
pathTerm    : pathFactor+ ;
pathFactor  : pathPrimary | pathPrimary graphPatternQuantifier | pathPrimary QUESTION_MARK ;
pathPrimary : elementPattern | parenthesizedPathPatternExpression | simplifiedPathPatternExpression ;
elementPattern : nodePattern | edgePattern ;
```

So a linear path is just a *sequence* of node/edge element patterns
(`pathTerm : pathFactor+`) — node/edge alternation is **semantic**, not
grammatical; the parser should enforce node-edge-node alternation itself
(matching ISO semantics) with good errors. `parenthesizedPathPatternExpression`
(line 1088) and `simplifiedPathPatternExpression` (16.12 — the `-/ … /->`
slash forms) are v1 reject.

### Node pattern (lines 993–1033)

```
nodePattern          : LEFT_PAREN elementPatternFiller RIGHT_PAREN ;
elementPatternFiller : elementVariableDeclaration? isLabelExpression? elementPatternPredicate? ;
isLabelExpression    : isOrColon labelExpression ;       // isOrColon : IS | COLON
elementPatternPredicate : elementPatternWhereClause | elementPropertySpecification ;
elementPatternWhereClause     : WHERE searchCondition ;
elementPropertySpecification  : LEFT_BRACE propertyKeyValuePairList RIGHT_BRACE ;
propertyKeyValuePair : propertyName COLON valueExpression ;
```

- All three filler parts are optional ⇒ `()` is a valid node pattern.
- Labels introduced by `:` **or** the keyword `IS` (`(a IS person)` ≡
  `(a:person)`).
- A filler has *either* an inline `WHERE` *or* a property map `{k: v, …}`,
  **never both** (single optional `elementPatternPredicate`).

### Edge pattern (lines 1035–1086)

`edgePattern : fullEdgePattern | abbreviatedEdgePattern ;` — the full forms
wrap the same `elementPatternFiller` as nodes:

| Production | Syntax | Meaning |
|---|---|---|
| `fullEdgePointingLeft` | `<-[ filler ]-` | directed, leftward |
| `fullEdgeUndirected` | `~[ filler ]~` | undirected |
| `fullEdgePointingRight` | `-[ filler ]->` | directed, rightward |
| `fullEdgeLeftOrUndirected` | `<~[ filler ]~` | left or undirected |
| `fullEdgeUndirectedOrRight` | `~[ filler ]~>` | undirected or right |
| `fullEdgeLeftOrRight` | `<-[ filler ]->` | left or right |
| `fullEdgeAnyDirection` | `-[ filler ]-` | any direction |

Abbreviated forms (`abbreviatedEdgePattern`, line 1078), same order:
`<-` · `~` · `->` · `<~` · `~>` · `<->` · `-`.

The arrow brackets are **single lexer tokens**: `<-[`, `~[`, `-[`, `<~[`,
`]->`, `]-`, `]~`, `]~>` (lines 3631–3658). The lexer must emit these
compound tokens (longest-match) — the parser never assembles them from `<`,
`-`, `[`.

v1 accepts: `-[f]->`, `<-[f]-`, `->`, `<-` (parse all seven forms, but
lowering rejects the any-direction `-[f]-`/`-` forms together with the
undirected/mixed `~` forms and `<->`, each with precise spans).

### Label expressions (16.8, lines 1102–1109)

```
labelExpression : EXCLAMATION_MARK labelExpression   #Negation        // !A
                | labelExpression AMPERSAND labelExpression  #Conjunction  // A&B
                | labelExpression VERTICAL_BAR labelExpression #Disjunction // A|B
                | labelName                          #Name
                | PERCENT                            #Wildcard        // %
                | LEFT_PAREN labelExpression RIGHT_PAREN #Parenthesized ;
```

Precedence per ANTLR alternative order: `!` > `&` > `|`. v1 parses the full
expression but accepts only a single `labelName`; anything else
(`!`, `&`, `|`, `%`, parens) is a clean lowering rejection. There is **no**
Cypher-style `:A:B` conjunction (`:` only introduces the expression).

### Quantifiers (16.11, lines 1125–1146)

```
graphPatternQuantifier : ASTERISK | PLUS_SIGN | fixedQuantifier | generalQuantifier ;
fixedQuantifier        : LEFT_BRACE unsignedInteger RIGHT_BRACE ;          // {n}
generalQuantifier      : LEFT_BRACE lowerBound? COMMA upperBound? RIGHT_BRACE ; // {n,m} {n,} {,m} {,}
```

Postfix on a `pathFactor` (i.e. *after* the edge pattern: `-[:knows]->{1,3}`),
plus the separate `?` optional quantifier. Both bounds of `generalQuantifier`
are optional but the comma is required. `*` ≡ `{0,}`, `+` ≡ `{1,}`. v1: parse
all; lowering accepts only `{1}`/`{1,n}` — the minimum must be exactly one —
on a single edge-pattern (per plan, `*`/`+`/`?`/`{0,m}`/unbounded `{n,}`,
minimums greater than one, and edge variables or predicates on quantified
edges are rejected; see `LOWERING.md` §6 for why min ≥ 2 is deferred).

---

## (d) RETURN and ordering

From 14.10–14.11 (lines 660–685) and 14.9 / 16.16–16.19:

```
primitiveResultStatement : returnStatement orderByAndPageStatement? | FINISH ;
returnStatement          : RETURN returnStatementBody ;
returnStatementBody      : setQuantifier? (ASTERISK | returnItemList) groupByClause? ;
setQuantifier            : DISTINCT | ALL ;                    (line 2405)
returnItemList           : returnItem (COMMA returnItem)* ;
returnItem               : aggregatingValueExpression returnItemAlias? ;
returnItemAlias          : AS identifier ;
```

- `RETURN *` is `ASTERISK`; `RETURN DISTINCT a.x, b.y AS name` per the list.
  `ALL` is the explicit no-dedup quantifier (default). An attached
  `GROUP BY` (`groupByClause`, line 1313, grouping elements are *binding
  variable references*) is v1-reject. `FINISH` (no results) — v1 reject.
- Aliases are `AS identifier` — so delimited identifiers are valid aliases
  (`AS "full name"`).

```
orderByAndPageStatement : orderByClause offsetClause? limitClause?
                        | offsetClause limitClause?
                        | limitClause ;                        (line 652)
orderByClause      : ORDER BY sortSpecificationList ;          (line 1332)
sortSpecification  : sortKey orderingSpecification? nullOrdering? ;
orderingSpecification : ASC | ASCENDING | DESC | DESCENDING ;
nullOrdering       : NULLS FIRST | NULLS LAST ;
limitClause        : LIMIT nonNegativeIntegerSpecification ;
offsetClause       : offsetSynonym nonNegativeIntegerSpecification ;
offsetSynonym      : OFFSET | SKIP_RESERVED_WORD ;             // OFFSET ≡ SKIP
```

- Fixed clause order: `ORDER BY` → `OFFSET`/`SKIP` → `LIMIT`; each later
  clause may appear without the earlier ones, but never before them.
- `sortKey` is a full value expression; `NULLS FIRST|LAST` is grammatical —
  v1 may parse-and-reject `nullOrdering` if the engine mapping is deferred.
- `LIMIT`/`OFFSET` counts are an unsigned integer **or a `$param`**
  (`nonNegativeIntegerSpecification`, line 2268).
- `orderByAndPageStatement` is *also* a standalone `primitiveQueryStatement`
  (line 568) usable between MATCHes — v1 only supports it post-RETURN.

---

## (e) Value expression precedence

From the consolidated left-recursive `valueExpression` (20.1, lines
2137–2163; ANTLR semantics: earlier alternative = tighter binding), plus
`valueExpressionPrimary` (20.2, line 2220) and `predicate` (19.2, line 2008).
Lowest to highest:

| Level | Operators | Production / alt label | Notes |
|---|---|---|---|
| 1 (lowest) | `OR`, `XOR` | `#disjunctiveExprAlt` | same level, left-assoc |
| 2 | `AND` | `#conjunctiveExprAlt` | left-assoc |
| 3 | `IS [NOT] TRUE\|FALSE\|UNKNOWN` | `#isNotExprAlt` | postfix boolean test (`truthValue`, line 2536) |
| 4 | `NOT` | `#notExprAlt` | prefix |
| 5 | `IS [NOT] [form] NORMALIZED` | `#normalizedPredicateExprAlt` | postfix; v1 reject |
| 6 | `=` `<>` `<` `>` `<=` `>=` | `#comparisonExprAlt` / `compOp` (line 2025) | left-assoc in grammar (so `a=b=c` parses as `(a=b)=c`); **no chaining semantics — v1 should reject chained comparisons**. No `!=` token. |
| 7 | `\|\|` | `#concatenationExprAlt` | string/list concat |
| 8 | binary `+` `-` | `#addSubtractExprAlt` | |
| 9 | `*` `/` | `#multDivExprAlt` | |
| 10 | unary `+` `-` | `#signedExprAlt` | prefix |
| 11 (highest) | property access `.` , function calls, `(...)`, literals, `$param`, variables | `valueExpressionPrimary` (`valueExpressionPrimary PERIOD propertyName` = property reference, 20.11) | |

**IS NULL placement**: `IS [NOT] NULL` is **not** a precedence level. It is
`predicate → nullPredicate : valueExpressionPrimary IS NOT? NULL` (19.5,
lines 2042–2048), entering the expression via the non-recursive
`#predicateExprAlt`. Consequences:

- The left operand must be a **primary**: `a.x IS NOT NULL` is valid;
  `a.x + 1 IS NULL` is a syntax error — must be `(a.x + 1) IS NULL`.
- The whole `x IS NULL` term then composes normally:
  `a.x IS NULL AND b.y > 1` ≡ `(a.x IS NULL) AND (b.y > 1)`.
- Practical parse strategy: parse a primary/unary expression; if the next
  token is `IS`, dispatch on the following token (`NOT`/`NULL`/truth-value/
  `TYPED`/`NORMALIZED`/`LABELED`/`DIRECTED`…).

Other `predicate` alternatives (19.2): `existsPredicate` (`EXISTS { … }`),
`valueTypePredicate` (`x IS [NOT] TYPED t`), `directedPredicate`,
`labeledPredicate` (`x IS [NOT] LABELED l` / `x:l`),
`sourceDestinationPredicate`, `ALL_DIFFERENT(…)`, `SAME(…)`,
`PROPERTY_EXISTS(…)` — all parse-and-reject in v1. **There is no `IN`
membership predicate and no `LIKE`/`STARTS WITH`/`CONTAINS` predicate in the
grammar** (`IN` only appears in `FOR`/`LET … IN … END`; `LIKE` only in DDL
graph-type clauses, line 328).

`searchCondition : booleanValueExpression : valueExpression` (19.1, line
2002) — every WHERE (graph-pattern-level and element-pattern-level) is a full
value expression.

---

## (f) Deviations to watch (openCypher ≠ ISO GQL)

Places where Cypher habits would make the parser wrong:

1. **`--` is a line comment**, never an edge. Cypher `(a)--(b)` must be GQL
   `(a)-(b)` (any-direction `MINUS_SIGN` abbreviation). Lexer longest-match
   order matters: `-[` (`MINUS_LEFT_BRACKET`) vs `--` (comment) vs `-`.
2. **No `!=`** — only `<>`. (`!` exists solely in label expressions.) No
   regex `=~`, no `STARTS WITH` / `ENDS WITH` / `CONTAINS`.
3. **No `IN` list-membership operator** in expressions (Cypher's
   `x IN [1,2]`) — `IN` is reserved but only used by `FOR` and
   `LET … IN … END`.
4. **Label conjunction is `&`, not `:` chaining**: Cypher `(n:A:B)` is invalid
   GQL; write `(n:A&B)`. `IS` is an alternative introducer (`(n IS A)`).
   Wildcard `%` and negation `!` don't exist in (old) Cypher.
5. **Quantifier syntax and position**: GQL quantifies the whole edge pattern
   postfix — `-[:knows]->{1,3}`, `->*`, `->+`, `->?` — not Cypher's
   `-[:knows*1..3]->` inside the brackets. `{n,}`/`{,m}` open bounds allowed.
6. **Undirected `~` vs any-direction `-`** are distinct edge kinds in GQL
   (`fullEdgeUndirected` vs `fullEdgeAnyDirection`); Cypher only has the
   "either direction" dash. GQL also has mixed forms (`<~`, `~>`, `<->`).
7. **`SKIP` and `OFFSET` are both valid** (synonyms); Cypher only has `SKIP`.
   Clause order is fixed: ORDER BY → OFFSET/SKIP → LIMIT.
8. **Strings vs identifiers**: double quotes are *strings and identifiers*
   in GQL (context decides); Cypher reserves `"` for strings and backticks
   for identifiers. GQL also has `@'no escape'` mode and quote-doubling,
   which Cypher lacks.
9. **Three-valued boolean literal**: `UNKNOWN` is a boolean literal alongside
   TRUE/FALSE, and `IS [NOT] UNKNOWN` is a boolean test. Cypher has neither.
10. **`IS NULL` operand restriction** (§e): only a primary on the left —
    Cypher accepts any expression.
11. **MATCH-WHERE attaches to the graph pattern** (`graphPattern …
    graphPatternWhereClause?`); per-element inline `WHERE` lives *inside*
    node/edge fillers and is mutually exclusive with a property map. Cypher
    (pre-GQL versions) has no element-level WHERE and allows map + nothing
    else.
12. **`RETURN` may carry `GROUP BY`** (and a standalone `FILTER` statement
    exists instead of Cypher's `WITH … WHERE`); Cypher aggregates implicitly.
    v1 rejects both, but the parser must not treat `GROUP` as an identifier
    (it is reserved).
13. **Numeric literal extras**: `0o`/`0b` prefixes, `_` digit separators, and
    `M`/`F`/`D` suffixes don't exist in Cypher; conversely Cypher's legacy
    octal `010` is just decimal-with-leading-zero… which in GQL is plain
    `UNSIGNED_DECIMAL_INTEGER` too (no special leading-zero rule).
14. **Keywords are case-insensitive; the reserved list is huge** (§b) —
    common Cypher variable names like `count`, `exists`, `value`, `start`,
    `end`, `set`, `limit` are **reserved** in GQL and unusable as variables
    without delimiting.
