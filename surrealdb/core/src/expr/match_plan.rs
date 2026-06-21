//! The declarative IR for OpenGQL v2 `MATCH` queries.
//!
//! `MatchPlan` is the language-neutral binding-table plan node produced by the
//! GQL lowering and embedded into the logical plan as [`Expr::Match`]. The
//! streaming execution planner (`exec/planner/match_plan.rs`) compiles it into a
//! tree of physical operators; it never runs under the compute-only planner.
//!
//! See `doc/opengql/V2_DESIGN.md` §2 for the normative contract.

// The IR is constructed by the GQL lowering and consumed by the streaming
// planner; both land as sibling pieces of PR-A, so some constructors,
// accessors, and enum variants have no in-crate caller yet.
#![allow(dead_code)]

use surrealdb_types::{SqlFormat, ToSql};

use crate::expr::Expr;
use crate::val::TableName;

/// Index into [`MatchPlan::bindings`]; identifies a binding by position.
///
/// A raw `u32` index (not a newtype) by design: it is crate-private, never
/// crosses an API boundary, and the lowering guarantees every emitted id is a
/// valid `bindings` position (see the `MatchPlan` invariants below), so the
/// extra wrapping would buy no safety here.
pub(crate) type BindingId = u32;

/// The declarative plan for a single GQL `MATCH` query.
///
/// Invariants the lowering guarantees (the planner relies on these and never
/// re-derives them):
/// - every [`Expr`] is BINDING-ROW scoped (`a.x` → `Idiom[Field("a"),Field("x")]`), with 3VL guards
///   already inserted;
/// - [`MatchPredicate::deps`] is the exact set of bindings the expr reads;
/// - conjuncts are NNF-split and live on the clause whose pattern scope owns them (critical for
///   OPTIONAL);
/// - column names are final (naming rules applied; duplicates rejected);
/// - ORDER BY aliases are resolved (non-DISTINCT → source exprs; DISTINCT → columns);
/// - repeated pattern variables are rewritten to hidden bindings + equality conjuncts; anonymous
///   edges needing DIFFERENT-EDGES tracking have hidden bindings;
/// - every pattern is anchorable (rule in V2_DESIGN §0).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MatchPlan {
	/// All bindings; index equals the [`BindingId`].
	pub(crate) bindings: Vec<BindingDef>,
	/// Clauses in textual order; always at least one.
	pub(crate) clauses: Vec<MatchClausePlan>,
	pub(crate) output: MatchOutput,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BindingDef {
	pub(crate) name: String,
	pub(crate) kind: BindingKind,
	/// `true` if the user wrote this binding; `false` for hidden `__e<n>`/`__v<n>`.
	pub(crate) user_named: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum BindingKind {
	Node,
	Edge,
	EdgeGroup,
	Path,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MatchClausePlan {
	/// The all-or-nothing OPTIONAL block this clause belongs to (R3), or `None`
	/// for a mandatory clause. Whether a clause is optional is *exactly*
	/// `optional_group.is_some()` (see [`MatchClausePlan::is_optional`]) — the
	/// single source of truth, never a separate flag the two could drift apart.
	///
	/// Every clause lowered from one `OPTIONAL { MATCH …; MATCH … }` (or one plain
	/// `OPTIONAL MATCH …`, which is a block of one) shares the same id. The planner
	/// left-joins all clauses of one group as a SINGLE unit (it must NOT join them
	/// one inner clause at a time), so a block matches all-or-nothing; distinct ids
	/// on adjacent optional clauses mean they chain left-to-right as independent
	/// left-joins. The id is an opaque, per-query dense counter (group order ==
	/// textual order); only equality is meaningful.
	pub(crate) optional_group: Option<u32>,
	pub(crate) patterns: Vec<PatternPlan>,
	/// Clause-owned NNF conjuncts.
	pub(crate) predicates: Vec<MatchPredicate>,
}

impl MatchClausePlan {
	/// Whether this clause is the body of an `OPTIONAL` operand (a left-outer join
	/// against the accumulated binding table, R3) — exactly when it carries an
	/// [`MatchClausePlan::optional_group`] id.
	pub(crate) fn is_optional(&self) -> bool {
		self.optional_group.is_some()
	}
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PatternPlan {
	/// Binding for the whole path value (kind == [`BindingKind::Path`]).
	pub(crate) path_var: Option<BindingId>,
	pub(crate) start: NodeStep,
	/// Multi-hop chain: each step is an edge followed by the node it reaches.
	pub(crate) steps: Vec<(EdgeStep, NodeStep)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct NodeStep {
	pub(crate) binding: BindingId,
	pub(crate) label: Option<TableName>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct EdgeStep {
	/// Edge binding, or an [`BindingKind::EdgeGroup`] binding when quantified.
	pub(crate) binding: BindingId,
	pub(crate) label: Option<TableName>,
	pub(crate) direction: ExpandDirection,
	pub(crate) quantifier: Option<EdgeQuantifier>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ExpandDirection {
	Out,
	In,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct EdgeQuantifier {
	pub(crate) min: u32,
	pub(crate) max: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MatchPredicate {
	pub(crate) expr: Expr,
	/// Sorted and deduped set of bindings the expression reads.
	pub(crate) deps: Vec<BindingId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MatchOutput {
	/// Explicit output columns; `RETURN *` is pre-expanded by the lowering.
	pub(crate) columns: Vec<MatchColumn>,
	pub(crate) distinct: bool,
	/// Aggregation, if any. `None` means no aggregation (the columns project
	/// straight from the binding rows). `Some(keys)` means the binding table is
	/// folded by the (binding-row scoped) group-key expressions before
	/// projection — an empty `keys` is `GROUP ALL` (a single group over all
	/// rows, e.g. a bare `RETURN count(*)`). Each non-key column carries an
	/// aggregate; the planner inserts an `Aggregate` operator in place of the
	/// projection.
	pub(crate) group_by: Option<Vec<Expr>>,
	pub(crate) order: Vec<MatchOrder>,
	pub(crate) skip: Option<Expr>,
	pub(crate) limit: Option<Expr>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MatchColumn {
	pub(crate) name: String,
	pub(crate) expr: Expr,
	/// A sort-only column materialised by an aggregating query so a non-projected
	/// ORDER BY key (a grouping key, functionally-dependent value, or aggregate)
	/// can be sorted on; the planner emits it from the `Aggregate` operator and
	/// drops it with a final projection before the rows are returned. Always
	/// `false` for user-projected columns.
	pub(crate) hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MatchOrder {
	pub(crate) expr: Expr,
	pub(crate) ascending: bool,
}

impl MatchPlan {
	/// Look up a binding by id.
	///
	/// # Panics
	/// Panics if `id` is not a valid index into [`MatchPlan::bindings`]. Callers
	/// (the streaming planner) only ever pass ids that the lowering placed into
	/// this same plan's `bindings`/patterns, so the index is always in range; an
	/// out-of-range id is an internal-consistency bug, not a user-reachable path.
	/// For the never-panicking rendering path use [`MatchPlan::binding_name`].
	pub(crate) fn binding(&self, id: BindingId) -> &BindingDef {
		&self.bindings[id as usize]
	}

	/// The name of a binding by id, or `"?"` for an out-of-range id.
	///
	/// Used by the never-panicking [`ToSql`] rendering, so it tolerates a
	/// malformed plan rather than indexing.
	fn binding_name(&self, id: BindingId) -> &str {
		self.bindings.get(id as usize).map(|b| b.name.as_str()).unwrap_or("?")
	}
}

impl EdgeQuantifier {
	/// `true` if this quantifier is exactly `{n}` (a fixed repeat count).
	pub(crate) fn is_exact(&self) -> bool {
		self.max == Some(self.min)
	}
}

impl ExpandDirection {
	/// The reverse direction (used when anchoring mid-pattern).
	pub(crate) fn reverse(self) -> Self {
		match self {
			ExpandDirection::Out => ExpandDirection::In,
			ExpandDirection::In => ExpandDirection::Out,
		}
	}
}

/// Deterministic GQL-ish rendering of a [`MatchPlan`].
///
/// Used by EXPLAIN, `Debug`-adjacent tooling, and logs: it must be stable and
/// must never panic on any plan, well-formed or not. Predicate, column, order,
/// and skip/limit slots delegate to [`Expr`]'s `ToSql` so 3VL guard shapes stay
/// textually diffable. The rendering is single-line regardless of `fmt`.
impl ToSql for MatchPlan {
	fn fmt_sql(&self, f: &mut String, _fmt: SqlFormat) {
		for clause in self.clauses.iter() {
			if clause.is_optional() {
				f.push_str("OPTIONAL ");
			}
			f.push_str("MATCH ");
			for (i, pattern) in clause.patterns.iter().enumerate() {
				if i > 0 {
					f.push_str(", ");
				}
				self.render_pattern(f, pattern);
			}
			if !clause.predicates.is_empty() {
				f.push_str(" WHERE ");
				for (i, predicate) in clause.predicates.iter().enumerate() {
					if i > 0 {
						f.push_str(" AND ");
					}
					predicate.expr.fmt_sql(f, SqlFormat::SingleLine);
				}
			}
			f.push(' ');
		}

		f.push_str("RETURN");
		if self.output.distinct {
			f.push_str(" DISTINCT");
		}
		for (i, column) in self.output.columns.iter().enumerate() {
			if i > 0 {
				f.push(',');
			}
			f.push(' ');
			column.expr.fmt_sql(f, SqlFormat::SingleLine);
			f.push_str(" AS ");
			f.push_str(&column.name);
		}

		// `Some([])` is `GROUP ALL` and renders nothing extra (the aggregates in
		// the columns already imply it); `Some(keys)` renders `GROUP BY …`.
		if let Some(keys) = self.output.group_by.as_ref()
			&& !keys.is_empty()
		{
			f.push_str(" GROUP BY");
			for (i, key) in keys.iter().enumerate() {
				if i > 0 {
					f.push(',');
				}
				f.push(' ');
				key.fmt_sql(f, SqlFormat::SingleLine);
			}
		}

		if !self.output.order.is_empty() {
			f.push_str(" ORDER BY");
			for (i, order) in self.output.order.iter().enumerate() {
				if i > 0 {
					f.push(',');
				}
				f.push(' ');
				order.expr.fmt_sql(f, SqlFormat::SingleLine);
				f.push_str(if order.ascending {
					" ASC"
				} else {
					" DESC"
				});
			}
		}

		if let Some(skip) = self.output.skip.as_ref() {
			f.push_str(" SKIP ");
			skip.fmt_sql(f, SqlFormat::SingleLine);
		}
		if let Some(limit) = self.output.limit.as_ref() {
			f.push_str(" LIMIT ");
			limit.fmt_sql(f, SqlFormat::SingleLine);
		}
	}
}

impl MatchPlan {
	/// Render one pattern as `p = (a:person)-[k:knows]->(b:person)`.
	fn render_pattern(&self, f: &mut String, pattern: &PatternPlan) {
		if let Some(path_var) = pattern.path_var {
			f.push_str(self.binding_name(path_var));
			f.push_str(" = ");
		}
		self.render_node(f, &pattern.start);
		for (edge, node) in pattern.steps.iter() {
			self.render_edge(f, edge);
			self.render_node(f, node);
		}
	}

	/// Render a node element as `(a:person)`, `(a)`, or `(:person)`.
	fn render_node(&self, f: &mut String, node: &NodeStep) {
		f.push('(');
		let def = self.bindings.get(node.binding as usize);
		if let Some(def) = def
			&& def.user_named
		{
			f.push_str(&def.name);
		}
		if let Some(label) = node.label.as_ref() {
			f.push(':');
			label.fmt_sql(f, SqlFormat::SingleLine);
		}
		f.push(')');
	}

	/// Render an edge element including direction arrows and any quantifier.
	fn render_edge(&self, f: &mut String, edge: &EdgeStep) {
		match edge.direction {
			ExpandDirection::Out => f.push('-'),
			ExpandDirection::In => f.push_str("<-"),
		}
		f.push('[');
		let def = self.bindings.get(edge.binding as usize);
		if let Some(def) = def
			&& def.user_named
		{
			f.push_str(&def.name);
		}
		if let Some(label) = edge.label.as_ref() {
			f.push(':');
			label.fmt_sql(f, SqlFormat::SingleLine);
		}
		f.push(']');
		match edge.direction {
			ExpandDirection::Out => f.push_str("->"),
			ExpandDirection::In => f.push('-'),
		}
		if let Some(quantifier) = edge.quantifier.as_ref() {
			self.render_quantifier(f, quantifier);
		}
	}

	/// Render a quantifier as the canonical brace form `{min,max}`.
	fn render_quantifier(&self, f: &mut String, quantifier: &EdgeQuantifier) {
		f.push('{');
		if quantifier.is_exact() {
			f.push_str(&quantifier.min.to_string());
		} else {
			f.push_str(&quantifier.min.to_string());
			f.push(',');
			if let Some(max) = quantifier.max {
				f.push_str(&max.to_string());
			}
		}
		f.push('}');
	}
}

#[cfg(test)]
mod tests {
	use surrealdb_types::ToSql;

	use super::*;
	use crate::expr::{Expr, Idiom, Literal, Part};

	/// Build `a.x` as a binding-row scoped idiom expression.
	fn field_path(binding: &str, field: &str) -> Expr {
		Expr::Idiom(Idiom(vec![
			Part::Field(binding.to_string().into()),
			Part::Field(field.to_string().into()),
		]))
	}

	/// `MATCH (a:person)-[k:knows]->(b:person) WHERE k.since > 2020
	///  RETURN a.name AS a_name, b.name AS b_name`
	fn sample_plan() -> MatchPlan {
		let bindings = vec![
			BindingDef {
				name: "a".to_string(),
				kind: BindingKind::Node,
				user_named: true,
			},
			BindingDef {
				name: "k".to_string(),
				kind: BindingKind::Edge,
				user_named: true,
			},
			BindingDef {
				name: "b".to_string(),
				kind: BindingKind::Node,
				user_named: true,
			},
		];
		let predicate = MatchPredicate {
			expr: Expr::Binary {
				left: Box::new(field_path("k", "since")),
				op: crate::expr::BinaryOperator::MoreThan,
				right: Box::new(Expr::Literal(Literal::Integer(2020))),
			},
			deps: vec![1],
		};
		let pattern = PatternPlan {
			path_var: None,
			start: NodeStep {
				binding: 0,
				label: Some(TableName::new("person".to_string())),
			},
			steps: vec![(
				EdgeStep {
					binding: 1,
					label: Some(TableName::new("knows".to_string())),
					direction: ExpandDirection::Out,
					quantifier: None,
				},
				NodeStep {
					binding: 2,
					label: Some(TableName::new("person".to_string())),
				},
			)],
		};
		MatchPlan {
			bindings,
			clauses: vec![MatchClausePlan {
				optional_group: None,
				patterns: vec![pattern],
				predicates: vec![predicate],
			}],
			output: MatchOutput {
				columns: vec![
					MatchColumn {
						name: "a_name".to_string(),
						expr: field_path("a", "name"),
						hidden: false,
					},
					MatchColumn {
						name: "b_name".to_string(),
						expr: field_path("b", "name"),
						hidden: false,
					},
				],
				distinct: false,
				group_by: None,
				order: Vec::new(),
				skip: None,
				limit: None,
			},
		}
	}

	#[test]
	fn to_sql_renders_single_pattern() {
		let plan = sample_plan();
		assert_eq!(
			plan.to_sql(),
			"MATCH (a:person)-[k:knows]->(b:person) WHERE k.since > 2020 RETURN a.name AS \
			 a_name, b.name AS b_name"
		);
	}

	#[test]
	fn to_sql_renders_distinct_order_quantifier_path() {
		let mut plan = sample_plan();
		// Promote to a path-var, quantified, distinct, ordered query.
		plan.bindings.push(BindingDef {
			name: "p".to_string(),
			kind: BindingKind::Path,
			user_named: true,
		});
		plan.bindings[1].kind = BindingKind::EdgeGroup;
		let path_id = (plan.bindings.len() - 1) as BindingId;
		let clause = &mut plan.clauses[0];
		clause.patterns[0].path_var = Some(path_id);
		clause.patterns[0].steps[0].0.quantifier = Some(EdgeQuantifier {
			min: 1,
			max: Some(3),
		});
		clause.predicates.clear();
		plan.output.distinct = true;
		plan.output.order = vec![MatchOrder {
			expr: field_path("a", "age"),
			ascending: false,
		}];
		plan.output.skip = Some(Expr::Literal(Literal::Integer(5)));
		plan.output.limit = Some(Expr::Literal(Literal::Integer(10)));

		assert_eq!(
			plan.to_sql(),
			"MATCH p = (a:person)-[k:knows]->{1,3}(b:person) RETURN DISTINCT a.name AS a_name, \
			 b.name AS b_name ORDER BY a.age DESC SKIP 5 LIMIT 10"
		);
	}

	#[test]
	fn to_sql_renders_hidden_bindings_anonymously() {
		let mut plan = sample_plan();
		// A hidden edge binding renders without its name.
		plan.bindings[1].user_named = false;
		plan.bindings[1].name = "__e0".to_string();
		plan.clauses[0].predicates.clear();
		plan.output.columns.truncate(1);
		assert_eq!(plan.to_sql(), "MATCH (a:person)-[:knows]->(b:person) RETURN a.name AS a_name");
	}

	#[test]
	fn debug_does_not_panic() {
		let plan = sample_plan();
		let rendered = format!("{plan:?}");
		assert!(rendered.contains("MatchPlan"));
	}

	#[test]
	fn binding_accessor_returns_def() {
		let plan = sample_plan();
		assert_eq!(plan.binding(1).name, "k");
		assert_eq!(plan.binding(1).kind, BindingKind::Edge);
	}

	#[test]
	fn to_sql_tolerates_out_of_range_binding() {
		// A malformed plan must still render without panicking.
		let mut plan = sample_plan();
		plan.clauses[0].patterns[0].start.binding = 99;
		let rendered = plan.to_sql();
		assert!(rendered.contains("(:person)"));
	}

	#[test]
	fn revisioned_serialize_of_match_fails_loud() {
		// `Expr::Match` must never enter Revisioned serialization (V2_DESIGN §2;
		// SECURITY_GUIDE §15a): its `to_sql()` renders GQL-ish text that the
		// SurrealQL-only deserialize path cannot round-trip. The serialize path
		// mirrors the `From<expr::Expr> for sql::Expr` arm and fails loud via
		// `debug_assert!`, so a future regression that nests `Expr::Match` is
		// caught in debug builds rather than silently emitting corrupt bytes.
		use revision::SerializeRevisioned;

		let expr = Expr::Match(Box::new(sample_plan()));
		let mut bytes = Vec::new();
		let serialize = std::panic::AssertUnwindSafe(|| {
			let _ = SerializeRevisioned::serialize_revisioned(&expr, &mut bytes);
		});
		let outcome = std::panic::catch_unwind(serialize);

		if cfg!(debug_assertions) {
			// Debug: the guard's `debug_assert!(false)` must trip.
			assert!(
				outcome.is_err(),
				"Expr::Match serialize_revisioned must panic in debug builds"
			);
		} else {
			// Release: no panic, but the bytes it would emit are GQL text that
			// is NOT valid SurrealQL — the invariant is upheld structurally
			// (this path is unreachable by construction), the guard is purely a
			// debug tripwire.
			assert!(outcome.is_ok());
		}
	}
}
