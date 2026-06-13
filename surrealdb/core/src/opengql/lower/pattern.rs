//! Pattern shape analysis, predicate placement and the SELECT scaffolding.
//!
//! Implements `doc/opengql/LOWERING.md` §2.1 (the degenerate single-node
//! SELECT and the three-level L1 bind / L2 unnest / L3 return shape), §3
//! (predicate placement), §6 (variable-length hops via `Part::Recurse`) and
//! the pattern-level rejections of §7.

use reblessive::Stk;

use crate::opengql::ast::{
	BinaryOp, EdgeDirection, EdgePattern, ElementPredicate, GqlExpr, Ident, LabelExpr, MatchClause,
	PathPattern, Quantifier, QuantifierKind, UnaryOp,
};
use crate::opengql::lower::expr::{self, Bindings, Role, Scope};
use crate::opengql::lower::naming;
use crate::sql::field::Selector;
use crate::sql::lookup::{Lookup, LookupKind, LookupSubject};
use crate::sql::part::{Recurse, RecurseInstruction};
use crate::sql::{
	BinaryOperator, Cond, Dir, Expr, Field, Fields, Function, FunctionCall, Idiom, Literal, Param,
	Part, SelectStatement, Split, Splits,
};
use crate::syn::error::{SyntaxError, bail, syntax_error};
use crate::val::TableName;

/// The validated, lowerable shape of the single path pattern.
pub(super) struct Shape {
	/// The anchor table: the label of the leftmost node.
	pub anchor_table: String,
	/// The single edge step, if the pattern has one.
	pub hop: Option<Hop>,
	/// The variable addressing facts.
	pub bindings: Bindings,
}

/// A single validated edge step.
pub(super) struct Hop {
	/// The lookup direction: `->` is `Out`, `<-` is `In`.
	pub dir: Dir,
	/// The edge table, when the edge pattern is labeled. An unlabeled edge
	/// scans all edge tables (`?`).
	pub edge_table: Option<String>,
	/// The far node's label: the `record::tb` filter for a single hop, or
	/// the recursion target table for a variable-length hop.
	pub far_table: Option<String>,
	/// `Some` for a variable-length hop (§6).
	pub recurse: Option<Recurse>,
}

/// Validates the path pattern of a MATCH clause and extracts its shape,
/// rejecting the §7 pattern constructs.
pub(super) fn analyze(clause: &MatchClause) -> Result<Shape, SyntaxError> {
	let pattern = match clause.patterns.as_slice() {
		[pattern] => pattern,
		[_, second, ..] => {
			bail!(
				"Comma-separated graph patterns are not supported yet",
				@second.start.span => "match a single path pattern"
			);
		}
		[] => {
			return Err(syntax_error!(
				"Internal error: MATCH clause without a pattern",
				@clause.span
			));
		}
	};
	if let Some(path_var) = &pattern.path_var {
		bail!("Path variables are not supported yet", @path_var.span);
	}
	if let [_, second, ..] = pattern.steps.as_slice() {
		bail!(
			"Multi-hop path patterns (more than one edge step) are not supported yet",
			@second.edge.span
		);
	}

	let Some(anchor_label) = label_name(&pattern.start.label)? else {
		bail!(
			"The anchor (leftmost) node of a path pattern must have a label",
			@pattern.start.span => "add a label: `(n:label)`"
		);
	};

	let mut vars: Vec<(String, Role)> = Vec::new();
	declare_var(&mut vars, &pattern.start.var, Role::Anchor)?;

	let mut far_field = "out";
	let hop = match pattern.steps.first() {
		None => None,
		Some(step) => {
			let edge = &step.edge;
			let dir = match edge.direction {
				EdgeDirection::Right => Dir::Out,
				EdgeDirection::Left => {
					far_field = "in";
					Dir::In
				}
				EdgeDirection::Undirected
				| EdgeDirection::LeftOrUndirected
				| EdgeDirection::UndirectedOrRight
				| EdgeDirection::LeftOrRight
				| EdgeDirection::Any => {
					bail!(
						"Undirected and multi-directional edge patterns are not supported yet",
						@edge.span => "use a directed edge: `-[…]->` or `<-[…]-`"
					);
				}
			};
			let recurse = match &edge.quantifier {
				None => None,
				Some(quantifier) => Some(validate_quantifier(quantifier, edge)?),
			};
			declare_var(&mut vars, &edge.var, Role::Edge)?;
			declare_var(&mut vars, &step.node.var, Role::FarNode)?;
			Some(Hop {
				dir,
				edge_table: label_name(&edge.label)?.map(|l| l.name.clone()),
				far_table: label_name(&step.node.label)?.map(|l| l.name.clone()),
				recurse,
			})
		}
	};

	let var_length = hop.as_ref().is_some_and(|hop| hop.recurse.is_some());
	Ok(Shape {
		anchor_table: anchor_label.name.clone(),
		hop,
		bindings: Bindings {
			vars,
			far_field,
			var_length,
		},
	})
}

/// Extracts the single label name of a node or edge, rejecting label
/// expressions, which have no table mapping (§7).
fn label_name(label: &Option<LabelExpr>) -> Result<Option<&Ident>, SyntaxError> {
	match label {
		None => Ok(None),
		Some(LabelExpr::Name(ident)) => Ok(Some(ident)),
		Some(other) => {
			bail!(
				"Label expressions (`!`, `&`, `|`, `%`) are not supported yet",
				@other.span() => "use a single label name"
			);
		}
	}
}

/// Declares a pattern variable, validating its name and rejecting repeats
/// (a repeated variable would be a join, which v1 does not lower).
fn declare_var(
	vars: &mut Vec<(String, Role)>,
	var: &Option<Ident>,
	role: Role,
) -> Result<(), SyntaxError> {
	let Some(ident) = var else {
		return Ok(());
	};
	naming::validate_var(ident)?;
	if vars.iter().any(|(name, _)| *name == ident.name) {
		bail!(
			"Variable `{}` is declared more than once in the pattern",
			ident.name,
			@ident.span => "joins on a repeated variable are not supported yet"
		);
	}
	vars.push((ident.name.clone(), role));
	Ok(())
}

/// Validates a variable-length quantifier per §6: no edge variable, no edge
/// predicate, a minimum of exactly one and a bounded maximum. Minima above
/// one are rejected: the collect instruction returns *distinct reachable*
/// nodes, which for `min == 1` is the documented v1 deviation from GQL's
/// one-row-per-path semantics, but for `min > 1` has no defensible GQL
/// reading at all (which paths "count" is unobservable without path rows).
/// Supporting `min > 1` is deferred to the per-path-semantics work.
fn validate_quantifier(
	quantifier: &Quantifier,
	edge: &EdgePattern,
) -> Result<Recurse, SyntaxError> {
	if let Some(var) = &edge.var {
		bail!(
			"Variable-length edge patterns cannot declare an edge variable",
			@var.span => "remove the variable or the quantifier"
		);
	}
	if edge.predicate.is_some() {
		bail!(
			"Variable-length edge patterns cannot have a WHERE clause or property map",
			@edge.span => "remove the predicate or the quantifier"
		);
	}
	match quantifier.kind {
		QuantifierKind::Star => {
			bail!(
				"The `*` quantifier is not supported yet",
				@quantifier.span => "use a bounded quantifier with a minimum of one: `{{1,n}}`"
			);
		}
		QuantifierKind::Plus => {
			bail!(
				"The `+` quantifier is not supported yet",
				@quantifier.span => "use a bounded quantifier: `{{1,n}}`"
			);
		}
		QuantifierKind::Question => {
			bail!("The `?` quantifier is not supported yet", @quantifier.span);
		}
		QuantifierKind::Fixed(0) => {
			bail!(
				"Variable-length quantifiers must have a minimum of at least one",
				@quantifier.span
			);
		}
		QuantifierKind::Fixed(1) => Ok(Recurse::Fixed(1)),
		QuantifierKind::Fixed(_) => {
			bail!(
				"Variable-length quantifiers with a minimum greater than one are not supported yet",
				@quantifier.span => "only `{{1}}` and `{{1,n}}` quantifiers are supported"
			);
		}
		QuantifierKind::Range(min, max) => {
			let Some(min @ 1..) = min else {
				bail!(
					"Variable-length quantifiers must have a minimum of at least one",
					@quantifier.span => "use `{{1,n}}` instead of `{{0,n}}`"
				);
			};
			let Some(max) = max else {
				bail!(
					"Unbounded variable-length quantifiers are not supported yet",
					@quantifier.span => "give the quantifier an upper bound: `{{1,n}}`"
				);
			};
			if max < min {
				bail!(
					"The quantifier maximum must not be smaller than its minimum",
					@quantifier.span
				);
			}
			if min > 1 {
				bail!(
					"Variable-length quantifiers with a minimum greater than one are not supported yet",
					@quantifier.span => "only `{{1}}` and `{{1,n}}` quantifiers are supported"
				);
			}
			Ok(Recurse::Range(Some(min), Some(max)))
		}
	}
}

/// A single predicate conjunct awaiting placement (§3).
pub(super) enum Conjunct<'a> {
	/// A user-written predicate, with the pending NNF negation.
	Expr {
		expr: &'a GqlExpr,
		negated: bool,
	},
	/// A property-map equality attached to a pattern element, kept
	/// role-addressed so that elements without a variable still lower.
	Prop {
		role: Role,
		key: &'a Ident,
		value: &'a GqlExpr,
	},
}

/// Merges the predicate sources in contract order — the explicit pattern
/// WHERE, then the inline node/edge WHEREs, then the property-map
/// equalities — splitting each into top-level conjuncts (§3 steps 1-3).
pub(super) fn collect_conjuncts<'a>(
	where_clause: Option<&'a GqlExpr>,
	pattern: &'a PathPattern,
) -> Vec<Conjunct<'a>> {
	let mut elements: Vec<(Role, &ElementPredicate)> = Vec::new();
	if let Some(predicate) = &pattern.start.predicate {
		elements.push((Role::Anchor, predicate));
	}
	if let Some(step) = pattern.steps.first() {
		if let Some(predicate) = &step.edge.predicate {
			elements.push((Role::Edge, predicate));
		}
		if let Some(predicate) = &step.node.predicate {
			elements.push((Role::FarNode, predicate));
		}
	}

	let mut out = Vec::new();
	if let Some(where_clause) = where_clause {
		split_conjuncts(where_clause, &mut out);
	}
	for (_, predicate) in &elements {
		if let ElementPredicate::Where(expr) = predicate {
			split_conjuncts(expr, &mut out);
		}
	}
	for (role, predicate) in &elements {
		if let ElementPredicate::Props(props) = predicate {
			for (key, value) in props {
				out.push(Conjunct::Prop {
					role: *role,
					key,
					value,
				});
			}
		}
	}
	out
}

/// Splits an expression into top-level conjuncts, pushing `NOT` through
/// `AND`/`OR` (De Morgan) so that conjuncts hidden under negation are
/// classified independently. ORs are never distributed (§3 step 2).
fn split_conjuncts<'a>(expr: &'a GqlExpr, out: &mut Vec<Conjunct<'a>>) {
	let mut stack: Vec<(&'a GqlExpr, bool)> = vec![(expr, false)];
	while let Some((expr, negated)) = stack.pop() {
		match expr {
			GqlExpr::Unary {
				op: UnaryOp::Not,
				expr,
				..
			} => stack.push((expr, !negated)),
			GqlExpr::Binary {
				op: BinaryOp::And,
				left,
				right,
				..
			} if !negated => {
				stack.push((right, negated));
				stack.push((left, negated));
			}
			GqlExpr::Binary {
				op: BinaryOp::Or,
				left,
				right,
				..
			} if negated => {
				stack.push((right, negated));
				stack.push((left, negated));
			}
			_ => out.push(Conjunct::Expr {
				expr,
				negated,
			}),
		}
	}
}

/// Where a conjunct is evaluated (§3).
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub(super) enum Placement {
	/// The anchor `WHERE` (L1, or the single SELECT of the no-edge shape).
	Anchor,
	/// The L1 lookup `cond` (per-edge filtering).
	Edge,
	/// The residual post-split `WHERE` (L3).
	PostSplit,
}

/// Classifies a conjunct by the variables it references: anchor-only
/// conjuncts filter the anchor scan; conjuncts touching the edge or far
/// node are pushed into the lookup `cond` when expressible there (property
/// accesses on any element, plus the anchor itself via `$parent`); anything
/// else lands in the residual post-split `WHERE`, which is always
/// semantically valid (§3).
pub(super) fn classify(
	conjunct: &Conjunct<'_>,
	bindings: &Bindings,
) -> Result<Placement, SyntaxError> {
	let mut anchor_only = true;
	let mut edge_ok = true;
	let expr = match conjunct {
		Conjunct::Expr {
			expr,
			..
		} => *expr,
		Conjunct::Prop {
			role,
			value,
			..
		} => {
			if *role != Role::Anchor {
				anchor_only = false;
			}
			*value
		}
	};
	walk_variables(expr, &mut |ident, bare| {
		let role = bindings.resolve(ident)?;
		if role != Role::Anchor {
			anchor_only = false;
			if bare {
				// A bare edge/far-node reference yields the full record,
				// which the lookup scope cannot address.
				edge_ok = false;
			}
		}
		Ok(())
	})?;
	if anchor_only {
		Ok(Placement::Anchor)
	} else if edge_ok && !bindings.var_length {
		Ok(Placement::Edge)
	} else {
		Ok(Placement::PostSplit)
	}
}

/// Lowers a conjunct in the scope its placement selected.
pub(super) async fn lower_conjunct(
	stk: &mut Stk,
	conjunct: &Conjunct<'_>,
	scope: &Scope<'_>,
) -> Result<Expr, SyntaxError> {
	match conjunct {
		Conjunct::Expr {
			expr: predicate,
			negated,
		} => expr::lower_predicate(stk, predicate, *negated, scope).await,
		Conjunct::Prop {
			role,
			key,
			value,
		} => expr::lower_prop_equality(stk, *role, key, value, scope).await,
	}
}

/// Visits every variable reference in an expression, flagging whether the
/// reference is bare or the base of a property access chain. Iterative: the
/// parser builds arbitrarily deep linear chains.
fn walk_variables<'a>(
	expr: &'a GqlExpr,
	visit: &mut impl FnMut(&'a Ident, bool) -> Result<(), SyntaxError>,
) -> Result<(), SyntaxError> {
	let mut stack = vec![expr];
	while let Some(e) = stack.pop() {
		match e {
			GqlExpr::Variable(ident) => visit(ident, true)?,
			GqlExpr::Property(base, _, _) => {
				let mut base = &**base;
				while let GqlExpr::Property(inner, _, _) = base {
					base = inner;
				}
				if let GqlExpr::Variable(ident) = base {
					visit(ident, false)?;
				} else {
					stack.push(base);
				}
			}
			GqlExpr::Unary {
				expr,
				..
			}
			| GqlExpr::IsBool {
				expr,
				..
			}
			| GqlExpr::IsNull {
				expr,
				..
			} => stack.push(expr),
			GqlExpr::Binary {
				left,
				right,
				..
			} => {
				stack.push(right);
				stack.push(left);
			}
			GqlExpr::FunctionCall {
				args,
				..
			} => stack.extend(args.iter()),
			GqlExpr::List(items, _) => stack.extend(items.iter()),
			GqlExpr::Map(fields, _) => stack.extend(fields.iter().map(|(_, value)| value)),
			GqlExpr::Literal(..)
			| GqlExpr::Param {
				..
			} => {}
		}
	}
	Ok(())
}

/// The b-side label filter `record::tb(out|in) = '<label>'` (§2.1), appended
/// after the user's edge-scope conjuncts (§8 example 4). Variable-length
/// hops target the far table in the recursion nest instead.
pub(super) fn far_label_filter(shape: &Shape) -> Option<Expr> {
	let hop = shape.hop.as_ref()?;
	if hop.recurse.is_some() {
		return None;
	}
	let label = hop.far_table.as_ref()?;
	Some(Expr::Binary {
		left: Box::new(Expr::FunctionCall(Box::new(FunctionCall {
			receiver: Function::Normal("record::tb".to_owned()),
			arguments: vec![Expr::Idiom(Idiom::field(shape.bindings.far_field))],
		}))),
		op: BinaryOperator::Equal,
		right: Box::new(Expr::Literal(Literal::String(label.clone().into()))),
	})
}

/// A `SELECT` with the §9 defaults: every clause not driven by the GQL
/// query matches what the SurrealQL parser produces for a plain SELECT.
fn select_defaults() -> SelectStatement {
	SelectStatement {
		fields: Fields::all(),
		omit: Vec::new(),
		only: false,
		what: Vec::new(),
		with: None,
		cond: None,
		split: None,
		group: None,
		order: None,
		limit: None,
		start: None,
		fetch: None,
		version: Expr::Literal(Literal::None),
		timeout: Expr::Literal(Literal::None),
		explain: None,
		tempfiles: false,
	}
}

/// Builds the SELECT scaffolding for the shape and returns the outermost
/// (projection) layer: the single SELECT of the degenerate no-edge shape,
/// or the L3 layer wrapping the L1 bind and L2 unnest layers (§2.1).
pub(super) fn build_frame(
	shape: &Shape,
	anchor_cond: Option<Expr>,
	edge_cond: Option<Expr>,
	post_cond: Option<Expr>,
) -> SelectStatement {
	let anchor = Expr::Table(TableName::new(shape.anchor_table.clone()));
	let Some(hop) = &shape.hop else {
		// Without a hop there is no edge or far-node role, so classification
		// can only have produced anchor conjuncts; a violation here would
		// silently drop user predicates.
		debug_assert!(
			edge_cond.is_none() && post_cond.is_none(),
			"no-hop shape cannot carry edge or post-split predicates"
		);
		let mut select = select_defaults();
		select.what = vec![anchor];
		select.cond = anchor_cond.map(Cond);
		return select;
	};

	// L1 "bind": one row per anchor, `__m` bound to the matching edges. The
	// inline all-fields projection makes the lookup yield full edge objects
	// (E1); a variable-length hop recurses from `id` instead, collecting
	// the distinct reachable nodes (§6 — a documented deviation from GQL's
	// one-row-per-path semantics).
	let hop_value = match &hop.recurse {
		None => Expr::Idiom(Idiom(vec![Part::Graph(Box::new(Lookup {
			kind: LookupKind::Graph(hop.dir.clone()),
			expr: Some(Fields::all()),
			what: subjects(&hop.edge_table),
			cond: edge_cond.map(Cond),
			..Default::default()
		}))])),
		Some(recurse) => {
			let nest = Idiom(vec![
				Part::Graph(Box::new(Lookup {
					kind: LookupKind::Graph(hop.dir.clone()),
					what: subjects(&hop.edge_table),
					..Default::default()
				})),
				Part::Graph(Box::new(Lookup {
					kind: LookupKind::Graph(hop.dir.clone()),
					what: subjects(&hop.far_table),
					..Default::default()
				})),
			]);
			Expr::Idiom(Idiom(vec![
				Part::Field("id".into()),
				Part::Recurse(
					recurse.clone(),
					Some(nest),
					Some(RecurseInstruction::Collect {
						inclusive: false,
					}),
				),
			]))
		}
	};
	let mut bind = select_defaults();
	bind.fields = Fields::Select(vec![
		Field::Single(Selector {
			expr: Expr::Param(Param::new("this")),
			alias: Some(Idiom::field("__a")),
		}),
		Field::Single(Selector {
			expr: hop_value,
			alias: Some(Idiom::field("__m")),
		}),
	]);
	bind.what = vec![anchor];
	bind.cond = anchor_cond.map(Cond);

	// L2 "unnest": one row per (anchor, edge). `SELECT *` so both engines
	// split the same docs; the `__m != []` guard enforces inner-join
	// semantics under the streaming engine, which passes empty arrays
	// through a SPLIT (E2).
	let mut unnest = select_defaults();
	unnest.what = vec![Expr::Select(Box::new(bind))];
	unnest.cond = Some(Cond(Expr::Binary {
		left: Box::new(Expr::Idiom(Idiom::field("__m"))),
		op: BinaryOperator::NotEqual,
		right: Box::new(Expr::Literal(Literal::Array(Vec::new()))),
	}));
	unnest.split = Some(Splits(vec![Split(Idiom::field("__m"))]));

	// L3 "return": residual predicates, projection, DISTINCT, ORDER, paging.
	let mut select = select_defaults();
	select.what = vec![Expr::Select(Box::new(unnest))];
	select.cond = post_cond.map(Cond);
	select
}

/// The lookup subjects for an optionally labeled element: an unlabeled
/// element scans all tables (`?`).
fn subjects(table: &Option<String>) -> Vec<LookupSubject> {
	table
		.as_ref()
		.map(|table| LookupSubject::Table {
			table: TableName::new(table.clone()),
			referencing_field: None,
		})
		.into_iter()
		.collect()
}
