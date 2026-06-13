//! Snapshot tests for the GQL → SurrealQL lowering.
//!
//! Each lowering test pins the exact `ToSql` rendering of the lowered AST,
//! verified against the construction rules of `doc/opengql/LOWERING.md`
//! (the §8 worked examples are the first group). Each rejection test pins
//! both the error message and the source slice its span covers.

use surrealdb_types::ToSql;

use crate::opengql::{GqlParserSettings, lower, parse_str, parse_to_ast_with_settings};

/// Parses and lowers a GQL query, returning the `ToSql` rendering of the
/// lowered AST.
fn lower_sql(source: &str) -> String {
	let query = match parse_str(source) {
		Ok(query) => query,
		Err(e) => panic!("failed to parse {source:?}: {:?}", e.render_on(source)),
	};
	match lower(query) {
		Ok(ast) => ast.to_sql(),
		Err(e) => panic!("failed to lower {source:?}: {:?}", e.render_on(source)),
	}
}

/// Parses successfully, then lowers expecting an error; returns the
/// rendered error and the source slices covered by the error's spans.
fn lower_err(source: &str) -> (String, Vec<String>) {
	let query = match parse_str(source) {
		Ok(query) => query,
		Err(e) => panic!("failed to parse {source:?}: {:?}", e.render_on(source)),
	};
	let error = lower(query).expect_err("lowering should have failed");
	let rendered = format!("{:?}", error.render_on(source));
	let mut slices = Vec::new();
	error.update_spans(|span| {
		let range = span.to_range();
		slices.push(source[range.start as usize..range.end as usize].to_owned());
	});
	(rendered, slices)
}

/// Asserts that lowering fails with a message containing `message` and a
/// span covering exactly `slice` in the source.
#[track_caller]
fn assert_rejects(source: &str, message: &str, slice: &str) {
	let (rendered, slices) = lower_err(source);
	assert!(
		rendered.contains(message),
		"error for {source:?} does not contain {message:?}: {rendered}"
	);
	assert!(
		slices.iter().any(|s| s == slice),
		"error spans for {source:?} cover {slices:?}, not {slice:?}"
	);
}

// ------------------------------------------------------------------------
// The §8 worked examples.
// ------------------------------------------------------------------------

#[test]
fn ex1_single_node_bare_variable() {
	assert_eq!(lower_sql("MATCH (n:person) RETURN n"), "SELECT $this AS n FROM person;");
}

#[test]
fn ex2_single_node_where_order_skip_limit() {
	assert_eq!(
		lower_sql(
			"MATCH (n:person) WHERE n.age > 18 RETURN n.name AS name ORDER BY name SKIP 5 LIMIT 10"
		),
		"SELECT name AS name FROM person WHERE age != NONE AND age != NULL AND age > 18 ORDER BY name LIMIT 10 START 5;"
	);
}

#[test]
fn ex3_one_hop_unaliased_dotted_columns() {
	assert_eq!(
		lower_sql("MATCH (a:person)-[:knows]->(b:person) RETURN a.name, b.name"),
		"SELECT __a.name AS `a.name`, __m.out.name AS `b.name` FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM knows WHERE record::tb(out) = 'person') AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn ex4_one_hop_edge_predicate_bare_variables() {
	assert_eq!(
		lower_sql("MATCH (a:person)-[k:knows]->(b:person) WHERE k.since > 2020 RETURN a, k, b"),
		"SELECT __a AS a, __m AS k, __m.out.* AS b FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM knows WHERE since != NONE AND since != NULL AND since > 2020 AND record::tb(out) = 'person') AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn ex5_property_map_literal_no_guard() {
	assert_eq!(
		lower_sql("MATCH (n:person {city: 'London'}) RETURN n"),
		"SELECT $this AS n FROM person WHERE city = 'London';"
	);
}

#[test]
fn ex6_var_length_range() {
	assert_eq!(
		lower_sql("MATCH (a:person)-[:knows]->{1,3}(b:person) RETURN b"),
		"SELECT __m.* AS b FROM (SELECT * FROM (SELECT $this AS __a, id.{1..3+collect}(->knows->person) AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn ex7_distinct_dotted_alias() {
	assert_eq!(
		lower_sql("MATCH (a:person)-[k:knows]->(b:person) RETURN DISTINCT b.name"),
		"SELECT __m.out.name AS `b.name` FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM knows WHERE record::tb(out) = 'person') AS __m FROM person) WHERE __m != [] SPLIT ON __m) GROUP BY `b.name`;"
	);
}

// ------------------------------------------------------------------------
// Shapes: directions, unlabeled elements, variable length.
// ------------------------------------------------------------------------

#[test]
fn left_direction_uses_in() {
	assert_eq!(
		lower_sql("MATCH (a:person)<-[k:knows]-(b:person) RETURN b.name"),
		"SELECT __m.in.name AS `b.name` FROM (SELECT * FROM (SELECT $this AS __a, <-(SELECT * FROM knows WHERE record::tb(in) = 'person') AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn unlabeled_edge_and_far_node() {
	// No edge table (`?` scans all edge tables), no `record::tb` filter.
	assert_eq!(
		lower_sql("MATCH (a:person)-[k]->(b) RETURN k"),
		"SELECT __m AS k FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM ?) AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn labeled_far_node_only() {
	assert_eq!(
		lower_sql("MATCH (a:person)-[k]->(b:person) RETURN k"),
		"SELECT __m AS k FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM ? WHERE record::tb(out) = 'person') AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn var_length_fixed() {
	assert_eq!(
		lower_sql("MATCH (a:person)-[:knows]->{1}(b:person) RETURN b"),
		"SELECT __m.* AS b FROM (SELECT * FROM (SELECT $this AS __a, id.{1+collect}(->knows->person) AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn var_length_left_direction() {
	assert_eq!(
		lower_sql("MATCH (a:person)<-[:knows]-{1,2}(b:person) RETURN b"),
		"SELECT __m.* AS b FROM (SELECT * FROM (SELECT $this AS __a, id.{1..2+collect}(<-knows<-person) AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn var_length_unlabeled_far_node() {
	assert_eq!(
		lower_sql("MATCH (a:person)-[:knows]->{1,2}(b) RETURN b"),
		"SELECT __m.* AS b FROM (SELECT * FROM (SELECT $this AS __a, id.{1..2+collect}(->knows->?) AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn var_length_property_access() {
	// §6: `__m` elements are RecordIds, so `b.x` is `__m.x` post-split.
	assert_eq!(
		lower_sql("MATCH (a:person)-[:knows]->{1,3}(b:person) RETURN b.name"),
		"SELECT __m.name AS `b.name` FROM (SELECT * FROM (SELECT $this AS __a, id.{1..3+collect}(->knows->person) AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

// ------------------------------------------------------------------------
// Predicate placement (§3).
// ------------------------------------------------------------------------

#[test]
fn anchor_only_conjunct_goes_to_l1() {
	assert_eq!(
		lower_sql("MATCH (a:person)-[k:knows]->(b:person) WHERE a.age > 18 RETURN k"),
		"SELECT __m AS k FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM knows WHERE record::tb(out) = 'person') AS __m FROM person WHERE age != NONE AND age != NULL AND age > 18) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn cross_variable_conjunct_pushed_to_edge_scope() {
	// `a.age > b.age` rewrites with `$parent`, with guards in edge scope
	// (E4 pins the unguarded hazard).
	assert_eq!(
		lower_sql("MATCH (a:person)-[k:knows]->(b:person) WHERE a.age > b.age RETURN k"),
		"SELECT __m AS k FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM knows WHERE $parent.age != NONE AND $parent.age != NULL AND out.age != NONE AND out.age != NULL AND $parent.age > out.age AND record::tb(out) = 'person') AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn bare_non_anchor_reference_falls_back_to_l3() {
	// A bare edge/far-node reference is not expressible in edge scope.
	assert_eq!(
		lower_sql("MATCH (a:person)-[k:knows]->(b:person) WHERE b = a RETURN k"),
		"SELECT __m AS k FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM knows WHERE record::tb(out) = 'person') AS __m FROM person) WHERE __m != [] SPLIT ON __m) WHERE __m.out.* = __a;"
	);
}

#[test]
fn or_conjunct_is_not_distributed() {
	// A top-level OR mixing anchor and edge references moves whole into
	// the lookup cond (all variables visible there); ORs never split.
	assert_eq!(
		lower_sql(
			"MATCH (a:person)-[k:knows]->(b:person) WHERE a.age > 60 OR k.since > 2020 RETURN k"
		),
		"SELECT __m AS k FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM knows WHERE ($parent.age != NONE AND $parent.age != NULL AND $parent.age > 60 OR since != NONE AND since != NULL AND since > 2020) AND record::tb(out) = 'person') AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn edge_property_map() {
	assert_eq!(
		lower_sql("MATCH (a:person)-[k:knows {since: 2020}]->(b:person) RETURN k"),
		"SELECT __m AS k FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM knows WHERE since = 2020 AND record::tb(out) = 'person') AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn far_node_property_map_goes_to_edge_scope() {
	assert_eq!(
		lower_sql("MATCH (a:person)-[:knows]->(b:person {city: 'London'}) RETURN a"),
		"SELECT __a AS a FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM knows WHERE out.city = 'London' AND record::tb(out) = 'person') AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn anonymous_far_node_property_map() {
	// Property maps lower role-addressed, so elements without a variable
	// still filter.
	assert_eq!(
		lower_sql("MATCH (a:person)-[:knows]->(:person {city: 'London'}) RETURN a"),
		"SELECT __a AS a FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM knows WHERE out.city = 'London' AND record::tb(out) = 'person') AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn inline_node_where() {
	assert_eq!(
		lower_sql("MATCH (n:person WHERE n.age > 18) RETURN n"),
		"SELECT $this AS n FROM person WHERE age != NONE AND age != NULL AND age > 18;"
	);
}

#[test]
fn inline_edge_where() {
	assert_eq!(
		lower_sql("MATCH (a:person)-[k:knows WHERE k.since > 2020]->(b:person) RETURN k"),
		"SELECT __m AS k FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM knows WHERE since != NONE AND since != NULL AND since > 2020 AND record::tb(out) = 'person') AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn inline_far_node_where_referencing_anchor() {
	assert_eq!(
		lower_sql("MATCH (a:person)-[:knows]->(b:person WHERE b.age < a.age) RETURN a"),
		"SELECT __a AS a FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM knows WHERE out.age != NONE AND out.age != NULL AND $parent.age != NONE AND $parent.age != NULL AND out.age < $parent.age AND record::tb(out) = 'person') AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn merge_order_explicit_where_then_inline_then_props() {
	assert_eq!(
		lower_sql("MATCH (n:person {city: 'London'} ) WHERE n.age > 18 RETURN n"),
		"SELECT $this AS n FROM person WHERE age != NONE AND age != NULL AND age > 18 AND city = 'London';"
	);
}

#[test]
fn var_length_far_node_predicate_goes_to_l3() {
	// The recursion hop has no lookup cond; non-anchor predicates land in
	// the residual post-split WHERE with `b.x` → `__m.x`.
	assert_eq!(
		lower_sql("MATCH (a:person)-[:knows]->{1,2}(b:person {city: 'London'}) RETURN b"),
		"SELECT __m.* AS b FROM (SELECT * FROM (SELECT $this AS __a, id.{1..2+collect}(->knows->person) AS __m FROM person) WHERE __m != [] SPLIT ON __m) WHERE __m.city = 'London';"
	);
}

#[test]
fn bare_anchor_in_anchor_scope_is_this() {
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE n = $x RETURN n"),
		"SELECT $this AS n FROM person WHERE $this = $x;"
	);
}

// ------------------------------------------------------------------------
// Three-valued logic guards and NNF (§4).
// ------------------------------------------------------------------------

#[test]
fn ordering_guard_param_operand() {
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE n.age = $min RETURN n"),
		"SELECT $this AS n FROM person WHERE age != NONE AND age != NULL AND $min != NONE AND $min != NULL AND age = $min;"
	);
}

#[test]
fn ordering_guard_both_params() {
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE $a < $b RETURN n"),
		"SELECT $this AS n FROM person WHERE $a != NONE AND $a != NULL AND $b != NONE AND $b != NULL AND $a < $b;"
	);
}

#[test]
fn ordering_guard_deduplicates_atoms() {
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE n.age > n.age RETURN n"),
		"SELECT $this AS n FROM person WHERE age != NONE AND age != NULL AND age > age;"
	);
}

#[test]
fn ordering_guard_arithmetic_operand() {
	// Guards apply to the nullable atoms inside composite operands.
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE n.age + 1 > 18 RETURN n"),
		"SELECT $this AS n FROM person WHERE age != NONE AND age != NULL AND age + 1 > 18;"
	);
}

#[test]
fn equality_with_literal_needs_no_guard() {
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE n.name = 'A' RETURN n"),
		"SELECT $this AS n FROM person WHERE name = 'A';"
	);
}

#[test]
fn equality_with_null_literal_is_guarded() {
	// Both sides nullable: the guards make `x = null` always false,
	// matching GQL's UNKNOWN-excluded semantics.
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE n.age = null RETURN n"),
		"SELECT $this AS n FROM person WHERE age != NONE AND age != NULL AND age = NULL;"
	);
}

#[test]
fn inequality_guards_one_sided() {
	// `NULL != 'A'` is true in SurrealQL but UNKNOWN in GQL, so `<>`
	// guards even when only one side is nullable.
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE n.name <> 'A' RETURN n"),
		"SELECT $this AS n FROM person WHERE name != NONE AND name != NULL AND name != 'A';"
	);
}

#[test]
fn guards_repeat_per_conjunct() {
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE n.age >= 18 AND n.age <= 65 RETURN n"),
		"SELECT $this AS n FROM person WHERE age != NONE AND age != NULL AND age >= 18 AND age != NONE AND age != NULL AND age <= 65;"
	);
}

#[test]
fn nnf_pushes_not_through_or() {
	// `NOT (a OR b)` → `NOT a AND NOT b`, split into two conjuncts, each
	// complemented and guarded.
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE NOT (n.age > 18 OR n.name = 'A') RETURN n"),
		"SELECT $this AS n FROM person WHERE age != NONE AND age != NULL AND age <= 18 AND name != NONE AND name != NULL AND name != 'A';"
	);
}

#[test]
fn nnf_pushes_not_through_and() {
	// `NOT (a AND b)` → `NOT a OR NOT b` — one conjunct, never distributed.
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE NOT (n.age > 18 AND n.flag) RETURN n"),
		"SELECT $this AS n FROM person WHERE age != NONE AND age != NULL AND age <= 18 OR flag = false;"
	);
}

#[test]
fn nnf_double_negation() {
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE NOT NOT n.flag RETURN n"),
		"SELECT $this AS n FROM person WHERE flag = true;"
	);
}

#[test]
fn nnf_not_complements_comparison() {
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE NOT n.age > 18 RETURN n"),
		"SELECT $this AS n FROM person WHERE age != NONE AND age != NULL AND age <= 18;"
	);
}

#[test]
fn bare_nullable_boolean_tests_true() {
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE n.flag RETURN n"),
		"SELECT $this AS n FROM person WHERE flag = true;"
	);
}

#[test]
fn not_bare_nullable_boolean_tests_false() {
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE NOT n.flag RETURN n"),
		"SELECT $this AS n FROM person WHERE flag = false;"
	);
}

#[test]
fn bare_param_predicate_tests_true() {
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE $flag RETURN n"),
		"SELECT $this AS n FROM person WHERE $flag = true;"
	);
}

#[test]
fn boolean_literal_predicate() {
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE true RETURN n"),
		"SELECT $this AS n FROM person WHERE true;"
	);
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE NOT true RETURN n"),
		"SELECT $this AS n FROM person WHERE false;"
	);
}

#[test]
fn is_null_test() {
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE n.age IS NULL RETURN n"),
		"SELECT $this AS n FROM person WHERE age = NULL OR age = NONE;"
	);
}

#[test]
fn is_not_null_test() {
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE n.age IS NOT NULL RETURN n"),
		"SELECT $this AS n FROM person WHERE age != NULL AND age != NONE;"
	);
}

#[test]
fn is_true_and_is_false_tests() {
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE n.flag IS TRUE RETURN n"),
		"SELECT $this AS n FROM person WHERE flag = true;"
	);
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE n.flag IS FALSE RETURN n"),
		"SELECT $this AS n FROM person WHERE flag = false;"
	);
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE n.flag IS NOT TRUE RETURN n"),
		"SELECT $this AS n FROM person WHERE flag != true;"
	);
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE n.flag IS NOT FALSE RETURN n"),
		"SELECT $this AS n FROM person WHERE flag != false;"
	);
}

#[test]
fn is_unknown_tests() {
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE n.flag IS UNKNOWN RETURN n"),
		"SELECT $this AS n FROM person WHERE flag = NULL OR flag = NONE;"
	);
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE n.flag IS NOT UNKNOWN RETURN n"),
		"SELECT $this AS n FROM person WHERE flag != NULL AND flag != NONE;"
	);
}

#[test]
fn not_negates_truth_test() {
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE NOT (n.flag IS TRUE) RETURN n"),
		"SELECT $this AS n FROM person WHERE flag != true;"
	);
	assert_eq!(
		lower_sql("MATCH (n:person) WHERE NOT (n.age IS NULL) RETURN n"),
		"SELECT $this AS n FROM person WHERE age != NULL AND age != NONE;"
	);
}

#[test]
fn guards_apply_in_edge_scope() {
	// E4 pins the hazard: `out.age < $parent.age` with a missing `out.age`
	// is TRUE unguarded.
	assert_eq!(
		lower_sql("MATCH (a:person)-[:knows]->(b:person) WHERE b.age < a.age RETURN a"),
		"SELECT __a AS a FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM knows WHERE out.age != NONE AND out.age != NULL AND $parent.age != NONE AND $parent.age != NULL AND out.age < $parent.age AND record::tb(out) = 'person') AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

// ------------------------------------------------------------------------
// RETURN, DISTINCT, ORDER BY, SKIP and LIMIT (§5).
// ------------------------------------------------------------------------

#[test]
fn return_star_single_node() {
	assert_eq!(lower_sql("MATCH (n:person) RETURN *"), "SELECT $this AS n FROM person;");
}

#[test]
fn return_star_expands_alphabetically() {
	assert_eq!(
		lower_sql("MATCH (b:person)-[a:knows]->(c:person) RETURN *"),
		"SELECT __m AS a, __a AS b, __m.out.* AS c FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM knows WHERE record::tb(out) = 'person') AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn return_star_unnamed_elements_skipped() {
	assert_eq!(
		lower_sql("MATCH (a:person)-[:knows]->(b:person) RETURN *"),
		"SELECT __a AS a, __m.out.* AS b FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM knows WHERE record::tb(out) = 'person') AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

#[test]
fn concat_lowers_to_add() {
	assert_eq!(
		lower_sql("MATCH (n:person) RETURN n.name || 'x' AS y"),
		"SELECT name + 'x' AS y FROM person;"
	);
}

#[test]
fn arithmetic_and_sign_operators() {
	assert_eq!(
		lower_sql("MATCH (n:person) RETURN n.age * 2 - 1 AS x, -n.age AS y"),
		"SELECT age * 2 - 1 AS x, -age AS y FROM person;"
	);
}

#[test]
fn list_and_map_literals() {
	assert_eq!(
		lower_sql("MATCH (n:person) RETURN [n.age, 1] AS lst, {a: n.age} AS mp"),
		"SELECT [age, 1] AS lst, { a: age } AS mp FROM person;"
	);
}

#[test]
fn comparison_in_value_position_is_unguarded() {
	assert_eq!(
		lower_sql("MATCH (n:person) RETURN n.age > 18 AS adult"),
		"SELECT age > 18 AS adult FROM person;"
	);
}

#[test]
fn distinct_groups_all_columns() {
	assert_eq!(
		lower_sql("MATCH (n:person) RETURN DISTINCT n.name, n.age ORDER BY n.name DESC"),
		"SELECT name AS `n.name`, age AS `n.age` FROM person GROUP BY `n.name`, `n.age` ORDER BY `n.name` DESC;"
	);
}

#[test]
fn order_by_alias() {
	assert_eq!(
		lower_sql("MATCH (n:person) RETURN n.name AS name ORDER BY name DESC"),
		"SELECT name AS name FROM person ORDER BY name DESC;"
	);
}

#[test]
fn order_by_verbatim_text_column() {
	// `a.name` matches the column named by the verbatim item text, so the
	// sort key is the dotted alias (E5).
	assert_eq!(
		lower_sql("MATCH (a:person)-[:knows]->(b:person) RETURN a.name ORDER BY a.name"),
		"SELECT __a.name AS `a.name` FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM knows WHERE record::tb(out) = 'person') AS __m FROM person) WHERE __m != [] SPLIT ON __m) ORDER BY `a.name`;"
	);
}

#[test]
fn order_by_aliased_item_resolves_to_alias() {
	// The sort key lowers to the same expression as the aliased item, so
	// it sorts on the item's column.
	assert_eq!(
		lower_sql("MATCH (n:person) RETURN n.name AS x ORDER BY n.name"),
		"SELECT name AS x FROM person ORDER BY x;"
	);
}

#[test]
fn order_by_non_return_expression_rejected() {
	// The legacy engine sorts projected output rows while the streaming
	// engine resolves source fields, so a sort key outside the RETURN
	// items would silently no-op sort under one strategy and sort under
	// another; only column-matching keys are accepted.
	assert_rejects(
		"MATCH (n:person) RETURN n.name ORDER BY n.age DESC",
		"ORDER BY may only reference RETURN items",
		"n.age DESC",
	);
}

#[test]
fn order_by_non_return_post_split_expression_rejected() {
	assert_rejects(
		"MATCH (a:person)-[k:knows]->(b:person) RETURN a ORDER BY k.since",
		"ORDER BY may only reference RETURN items",
		"k.since",
	);
}

#[test]
fn order_by_computed_return_item() {
	assert_eq!(
		lower_sql("MATCH (n:person) RETURN n.age + 1 ORDER BY n.age + 1"),
		"SELECT age + 1 AS `n.age + 1` FROM person ORDER BY `n.age + 1`;"
	);
}

#[test]
fn distinct_order_by_return_item_is_allowed() {
	assert_eq!(
		lower_sql("MATCH (n:person) RETURN DISTINCT n.name ORDER BY n.name"),
		"SELECT name AS `n.name` FROM person GROUP BY `n.name` ORDER BY `n.name`;"
	);
}

#[test]
fn skip_limit_parameters() {
	assert_eq!(
		lower_sql("MATCH (n:person) RETURN n SKIP $s LIMIT $l"),
		"SELECT $this AS n FROM person LIMIT $l START $s;"
	);
}

#[test]
fn offset_synonym_for_skip() {
	assert_eq!(
		lower_sql("MATCH (n:person) RETURN n OFFSET 2 LIMIT 3"),
		"SELECT $this AS n FROM person LIMIT 3 START 2;"
	);
}

#[test]
fn quoted_identifiers_lower_like_plain_ones() {
	// `"…"` is a delimited identifier in identifier positions, while in
	// expression position a variable reference must be accent-quoted.
	assert_eq!(
		lower_sql("MATCH (\"n\":person) RETURN `n`.name AS \"the name\""),
		"SELECT name AS `the name` FROM person;"
	);
}

#[test]
fn nested_property_chains() {
	assert_eq!(
		lower_sql("MATCH (n:person) RETURN n.address.city AS city"),
		"SELECT address.city AS city FROM person;"
	);
}

#[test]
fn nested_property_chains_post_split() {
	assert_eq!(
		lower_sql("MATCH (a:person)-[:knows]->(b:person) RETURN b.address.city AS city"),
		"SELECT __m.out.address.city AS city FROM (SELECT * FROM (SELECT $this AS __a, ->(SELECT * FROM knows WHERE record::tb(out) = 'person') AS __m FROM person) WHERE __m != [] SPLIT ON __m);"
	);
}

// ------------------------------------------------------------------------
// Public API entry points.
// ------------------------------------------------------------------------

#[test]
fn parse_to_ast_with_settings_end_to_end() {
	let ast = parse_to_ast_with_settings("MATCH (n:person) RETURN n", GqlParserSettings::default())
		.expect("should lower");
	assert_eq!(ast.to_sql(), "SELECT $this AS n FROM person;");
}

#[test]
fn parse_with_capabilities_renders_errors_like_surrealql() {
	use crate::cnf::CommonConfig;
	use crate::dbs::Capabilities;
	use crate::dbs::capabilities::Targets;
	let error = crate::opengql::parse_with_capabilities(
		"MATCH (n:person) RETURN m",
		&Capabilities::all().with_experimental(Targets::All),
		&CommonConfig::default(),
	)
	.expect_err("should fail");
	let rendered = format!("{error}");
	assert!(rendered.contains("Parse error"), "unexpected error: {rendered}");
	assert!(rendered.contains("Unknown variable `m`"), "unexpected error: {rendered}");
}

#[test]
fn parse_with_capabilities_enforces_the_experimental_gate() {
	use crate::cnf::CommonConfig;
	use crate::dbs::Capabilities;
	let error = crate::opengql::parse_with_capabilities(
		"MATCH (n:person) RETURN n",
		&Capabilities::all(),
		&CommonConfig::default(),
	)
	.expect_err("should fail");
	assert!(
		format!("{error}").contains("Experimental capability `opengql` is not enabled"),
		"unexpected error: {error}"
	);
}

#[test]
fn deep_linear_chains_lower_without_overflowing() {
	// The lowering must process linear chains (binary spines, `NOT`
	// prefixes, property postfixes) without machine-stack recursion. Such
	// chains now exceed the parse-time expression budget (default 128) by
	// design, so the deep cases raise the limit explicitly — the subject
	// under test is the lowering walk, which must stay safe for whatever
	// depth an operator's raised configuration admits. The NOT and property
	// chains lower to bounded/flat `sql` values, so an overflow here is the
	// lowering's own recursion; deep *binary* spines also produce equally
	// deep `sql::Expr` trees, whose recursive drop is a property shared
	// with syn-parsed SurrealQL at the same configured depth, so the spine
	// stays modest.
	let deep = GqlParserSettings {
		expr_recursion_limit: 100_000,
		..Default::default()
	};

	let nots = format!("MATCH (n:person) WHERE {} n.flag RETURN n", "NOT ".repeat(50_001));
	let ast = parse_to_ast_with_settings(&nots, deep.clone()).expect("NOT chain should lower");
	assert_eq!(ast.to_sql(), "SELECT $this AS n FROM person WHERE flag = false;");

	// Property chains are not charged against the expression budget.
	let props = format!("MATCH (n:person) RETURN n{} AS x", ".p".repeat(50_000));
	assert!(lower_sql(&props).starts_with("SELECT p.p.p"));

	let spine = format!("MATCH (n:person) RETURN 1{} AS x", " + 1".repeat(1_000));
	let ast = parse_to_ast_with_settings(&spine, deep).expect("binary spine should lower");
	assert!(ast.to_sql().starts_with("SELECT 1 + 1 + "));
}

// ------------------------------------------------------------------------
// Rejections (§7): message and span.
// ------------------------------------------------------------------------

#[test]
fn rejects_missing_match() {
	assert_rejects("RETURN 1", "A query without a MATCH clause is not supported yet", "RETURN 1");
}

#[test]
fn rejects_multiple_match_clauses() {
	assert_rejects(
		"MATCH (n:person) MATCH (m:person) RETURN n",
		"Multiple MATCH clauses are not supported yet",
		"MATCH (m:person)",
	);
}

#[test]
fn rejects_optional_match() {
	assert_rejects(
		"OPTIONAL MATCH (n:person) RETURN n",
		"OPTIONAL MATCH is not supported yet",
		"OPTIONAL MATCH (n:person)",
	);
}

#[test]
fn rejects_comma_separated_patterns() {
	assert_rejects(
		"MATCH (n:person), (m:person) RETURN n",
		"Comma-separated graph patterns are not supported yet",
		"(m:person)",
	);
}

#[test]
fn rejects_path_variables() {
	assert_rejects("MATCH p = (n:person) RETURN n", "Path variables are not supported yet", "p");
}

#[test]
fn rejects_multi_hop_patterns() {
	assert_rejects(
		"MATCH (a:person)-[:knows]->(b:person)-[:knows]->(c:person) RETURN a",
		"Multi-hop path patterns (more than one edge step) are not supported yet",
		"-[:knows]->",
	);
}

#[test]
fn rejects_undirected_edges() {
	assert_rejects(
		"MATCH (a:person)~(b:person) RETURN a",
		"Undirected and multi-directional edge patterns are not supported yet",
		"~",
	);
}

#[test]
fn rejects_any_direction_edges() {
	assert_rejects(
		"MATCH (a:person)-(b:person) RETURN a",
		"Undirected and multi-directional edge patterns are not supported yet",
		"-",
	);
}

#[test]
fn rejects_left_or_right_edges() {
	assert_rejects(
		"MATCH (a:person)<->(b:person) RETURN a",
		"Undirected and multi-directional edge patterns are not supported yet",
		"<->",
	);
}

#[test]
fn rejects_left_or_undirected_edges() {
	assert_rejects(
		"MATCH (a:person)<~(b:person) RETURN a",
		"Undirected and multi-directional edge patterns are not supported yet",
		"<~",
	);
}

#[test]
fn rejects_undirected_or_right_edges() {
	assert_rejects(
		"MATCH (a:person)~>(b:person) RETURN a",
		"Undirected and multi-directional edge patterns are not supported yet",
		"~>",
	);
}

#[test]
fn rejects_full_form_undirected_edges() {
	assert_rejects(
		"MATCH (a:person)-[k:knows]-(b:person) RETURN a",
		"Undirected and multi-directional edge patterns are not supported yet",
		"-[k:knows]-",
	);
}

#[test]
fn rejects_node_label_disjunction() {
	assert_rejects(
		"MATCH (n:person|admin) RETURN n",
		"Label expressions (`!`, `&`, `|`, `%`) are not supported yet",
		"person|admin",
	);
}

#[test]
fn rejects_node_label_wildcard() {
	assert_rejects(
		"MATCH (n:%) RETURN n",
		"Label expressions (`!`, `&`, `|`, `%`) are not supported yet",
		"%",
	);
}

#[test]
fn rejects_node_label_negation() {
	assert_rejects(
		"MATCH (n:!person) RETURN n",
		"Label expressions (`!`, `&`, `|`, `%`) are not supported yet",
		"!person",
	);
}

#[test]
fn rejects_edge_label_expression() {
	assert_rejects(
		"MATCH (a:person)-[k:knows|likes]->(b:person) RETURN a",
		"Label expressions (`!`, `&`, `|`, `%`) are not supported yet",
		"knows|likes",
	);
}

#[test]
fn rejects_unlabeled_anchor() {
	assert_rejects(
		"MATCH (n) RETURN n",
		"The anchor (leftmost) node of a path pattern must have a label",
		"(n)",
	);
}

#[test]
fn rejects_star_quantifier() {
	assert_rejects(
		"MATCH (a:person)-[:knows]->*(b:person) RETURN a",
		"The `*` quantifier is not supported yet",
		"*",
	);
}

#[test]
fn rejects_plus_quantifier() {
	assert_rejects(
		"MATCH (a:person)-[:knows]->+(b:person) RETURN a",
		"The `+` quantifier is not supported yet",
		"+",
	);
}

#[test]
fn rejects_question_quantifier() {
	assert_rejects(
		"MATCH (a:person)-[:knows]->?(b:person) RETURN a",
		"The `?` quantifier is not supported yet",
		"?",
	);
}

#[test]
fn rejects_zero_minimum_quantifier() {
	assert_rejects(
		"MATCH (a:person)-[:knows]->{0,3}(b:person) RETURN a",
		"Variable-length quantifiers must have a minimum of at least one",
		"{0,3}",
	);
}

#[test]
fn rejects_missing_minimum_quantifier() {
	assert_rejects(
		"MATCH (a:person)-[:knows]->{,3}(b:person) RETURN a",
		"Variable-length quantifiers must have a minimum of at least one",
		"{,3}",
	);
}

#[test]
fn rejects_zero_fixed_quantifier() {
	assert_rejects(
		"MATCH (a:person)-[:knows]->{0}(b:person) RETURN a",
		"Variable-length quantifiers must have a minimum of at least one",
		"{0}",
	);
}

#[test]
fn rejects_fixed_quantifier_above_one() {
	// The streaming collect BFS dedups nodes at first discovery, so a node
	// first reached below the minimum depth is dropped and the planner
	// strategies disagree on the result; minima above one are rejected
	// until that engine divergence is fixed and the behavior can be pinned.
	assert_rejects(
		"MATCH (a:person)-[:knows]->{2}(b:person) RETURN a",
		"Variable-length quantifiers with a minimum greater than one are not supported yet",
		"{2}",
	);
}

#[test]
fn rejects_range_quantifier_minimum_above_one() {
	assert_rejects(
		"MATCH (a:person)-[:knows]->{2,3}(b:person) RETURN a",
		"Variable-length quantifiers with a minimum greater than one are not supported yet",
		"{2,3}",
	);
}

#[test]
fn rejects_unbounded_quantifier() {
	assert_rejects(
		"MATCH (a:person)-[:knows]->{2,}(b:person) RETURN a",
		"Unbounded variable-length quantifiers are not supported yet",
		"{2,}",
	);
}

#[test]
fn rejects_empty_quantifier_range() {
	assert_rejects(
		"MATCH (a:person)-[:knows]->{3,2}(b:person) RETURN a",
		"The quantifier maximum must not be smaller than its minimum",
		"{3,2}",
	);
}

#[test]
fn rejects_quantifier_with_edge_variable() {
	assert_rejects(
		"MATCH (a:person)-[k:knows]->{1,2}(b:person) RETURN a",
		"Variable-length edge patterns cannot declare an edge variable",
		"k",
	);
}

#[test]
fn rejects_quantifier_with_edge_predicate() {
	assert_rejects(
		"MATCH (a:person)-[:knows {since: 2020}]->{1,2}(b:person) RETURN a",
		"Variable-length edge patterns cannot have a WHERE clause or property map",
		"-[:knows {since: 2020}]->{1,2}",
	);
}

#[test]
fn rejects_count_star() {
	assert_rejects(
		"MATCH (n:person) RETURN count(*)",
		"Aggregate functions are not supported yet",
		"count(*)",
	);
}

#[test]
fn rejects_count_distinct() {
	assert_rejects(
		"MATCH (n:person) RETURN count(DISTINCT n)",
		"Aggregate functions are not supported yet",
		"count(DISTINCT n)",
	);
}

#[test]
fn rejects_sum_aggregate() {
	assert_rejects(
		"MATCH (n:person) RETURN sum(n.age)",
		"Aggregate functions are not supported yet",
		"sum(n.age)",
	);
}

#[test]
fn rejects_non_aggregate_functions() {
	assert_rejects(
		"MATCH (n:person) RETURN upper(n.name)",
		"The function `upper` is not supported yet",
		"upper(n.name)",
	);
}

#[test]
fn rejects_function_calls_in_where() {
	assert_rejects(
		"MATCH (n:person) WHERE upper(n.name) = 'A' RETURN n",
		"The function `upper` is not supported yet",
		"upper(n.name)",
	);
}

#[test]
fn rejects_nulls_first() {
	assert_rejects(
		"MATCH (n:person) RETURN n.age ORDER BY n.age NULLS FIRST",
		"`NULLS FIRST`/`NULLS LAST` ordering is not supported yet",
		"n.age NULLS FIRST",
	);
}

#[test]
fn rejects_nulls_last() {
	assert_rejects(
		"MATCH (n:person) RETURN n.age ORDER BY n.age DESC NULLS LAST",
		"`NULLS FIRST`/`NULLS LAST` ordering is not supported yet",
		"n.age DESC NULLS LAST",
	);
}

#[test]
fn rejects_distinct_order_by_non_return_item() {
	assert_rejects(
		"MATCH (n:person) RETURN DISTINCT n.name ORDER BY n.age",
		"ORDER BY may only reference RETURN items",
		"n.age",
	);
}

#[test]
fn rejects_duplicate_verbatim_columns() {
	assert_rejects(
		"MATCH (n:person) RETURN n.name, n.name",
		"Duplicate column name `n.name`",
		"n.name",
	);
}

#[test]
fn rejects_duplicate_aliases() {
	assert_rejects(
		"MATCH (n:person) RETURN n.age AS x, n.name AS x",
		"Duplicate column name `x`",
		"x",
	);
}

#[test]
fn rejects_dunder_variables() {
	assert_rejects(
		"MATCH (__n:person) RETURN __n",
		"Variable names starting with `__` are reserved for internal use",
		"__n",
	);
}

#[test]
fn rejects_dunder_aliases() {
	assert_rejects(
		"MATCH (n:person) RETURN n.age AS __x",
		"Aliases starting with `__` are reserved for internal use",
		"__x",
	);
}

#[test]
fn rejects_dunder_parameters() {
	assert_rejects(
		"MATCH (n:person) WHERE n.age > $__p RETURN n",
		"Parameter names starting with `__` are reserved for internal use",
		"$__p",
	);
}

#[test]
fn rejects_engine_reserved_parameters() {
	assert_rejects(
		"MATCH (n:person) WHERE n.age > $parent RETURN n",
		"The parameter name `$parent` is reserved by the engine",
		"$parent",
	);
	assert_rejects(
		"MATCH (n:person) RETURN n LIMIT $auth",
		"The parameter name `$auth` is reserved by the engine",
		"$auth",
	);
}

#[test]
fn rejects_xor_in_predicates() {
	assert_rejects(
		"MATCH (n:person) WHERE n.flag XOR n.old RETURN n",
		"`XOR` is not supported yet",
		"n.flag XOR n.old",
	);
}

#[test]
fn rejects_xor_in_value_position() {
	assert_rejects(
		"MATCH (n:person) RETURN n.flag XOR true AS x",
		"`XOR` is not supported yet",
		"n.flag XOR true",
	);
}

#[test]
fn rejects_unknown_variables() {
	assert_rejects("MATCH (n:person) RETURN m", "Unknown variable `m`", "m");
	assert_rejects("MATCH (n:person) WHERE m.age > 1 RETURN n", "Unknown variable `m`", "m");
}

#[test]
fn rejects_repeated_variables() {
	assert_rejects(
		"MATCH (n:person)-[n]->(b:person) RETURN b",
		"Variable `n` is declared more than once in the pattern",
		"n",
	);
}

#[test]
fn rejects_return_star_without_variables() {
	assert_rejects(
		"MATCH (:person) RETURN *",
		"RETURN * requires at least one named pattern variable",
		"RETURN *",
	);
}
