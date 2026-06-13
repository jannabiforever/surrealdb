//! GQL expression lowering, parameterized by scope.
//!
//! Implements the variable addressing tables of `doc/opengql/LOWERING.md`
//! §2.2 and the three-valued-logic guard rules of §4. Predicates are
//! normalized to negation normal form on the fly: a `negated` flag is pushed
//! through `NOT`/`AND`/`OR` (De Morgan, never distributing ORs) and into the
//! comparison and truth-test leaves, where each effective comparison is
//! guarded so that a `NULL`/`NONE` operand excludes the row — matching GQL's
//! UNKNOWN-excluded `WHERE` semantics under SurrealQL's two-valued
//! total-order comparisons.
//!
//! All recursion over expressions runs on a [`reblessive`] stack: the parser
//! deliberately builds arbitrarily deep *linear* chains (binary operator
//! spines, property and `NOT` chains) without consuming its nesting budget,
//! so the lowering must not recurse on the machine stack either.

use reblessive::Stk;

use crate::opengql::ast::{
	BinaryOp, GqlExpr, GqlLiteral, Ident, SetQuantifier, TruthValue, UnaryOp,
};
use crate::opengql::lower::naming;
use crate::sql::literal::ObjectEntry;
use crate::sql::{BinaryOperator, Expr, Idiom, Literal, Param, Part, PrefixOperator};
use crate::syn::error::{SyntaxError, bail, syntax_error};
use crate::syn::token::Span;

/// The role a pattern variable plays in the lowered binding shape.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub(super) enum Role {
	/// The leftmost (anchor) node: the `FROM` table, bound as `__a`.
	Anchor,
	/// The edge of the hop, bound as `__m`.
	Edge,
	/// The node at the far end of the hop, derived from the edge.
	FarNode,
}

/// The named pattern variables of the lowered path pattern, plus the
/// structural facts variable addressing depends on.
pub(super) struct Bindings {
	/// `name → role`, in pattern order.
	pub vars: Vec<(String, Role)>,
	/// The edge field the far node is derived from: `out` for `->`, `in`
	/// for `<-`.
	pub far_field: &'static str,
	/// Whether the hop is variable-length (§6), in which case the `__m`
	/// elements are RecordIds rather than full edge objects.
	pub var_length: bool,
}

impl Bindings {
	/// Resolves a variable reference to its role.
	pub fn resolve(&self, ident: &Ident) -> Result<Role, SyntaxError> {
		self.vars.iter().find(|(name, _)| *name == ident.name).map(|(_, role)| *role).ok_or_else(
			|| {
				syntax_error!(
					"Unknown variable `{}`",
					ident.name,
					@ident.span => "variables must be declared in the MATCH pattern"
				)
			},
		)
	}
}

/// The scope an expression is lowered in, selecting one of the §2.2
/// addressing tables.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub(super) enum ScopeKind {
	/// The anchor SELECT: its `WHERE` in the hop shapes, and the entire
	/// single SELECT of the degenerate no-edge shape.
	Anchor,
	/// The `cond` of the L1 graph lookup (per-edge filtering).
	Edge,
	/// The post-split L3 layer: projections, residual predicates, ORDER BY.
	PostSplit,
}

pub(super) struct Scope<'a> {
	pub kind: ScopeKind,
	pub bindings: &'a Bindings,
}

impl Scope<'_> {
	/// Lowers a reference to a pattern variable, optionally suffixed with a
	/// property chain, per the addressing tables of §2.2.
	pub(super) fn role_expr(
		&self,
		role: Role,
		props: &[&Ident],
		span: Span,
	) -> Result<Expr, SyntaxError> {
		let far = self.bindings.far_field;
		let parts: Vec<Part> = match (self.kind, role) {
			(ScopeKind::Anchor, Role::Anchor) => {
				if props.is_empty() {
					return Ok(Expr::Param(Param::new("this")));
				}
				field_parts(props)
			}
			(ScopeKind::Edge, Role::Edge) if !props.is_empty() => field_parts(props),
			(ScopeKind::Edge, Role::FarNode) if !props.is_empty() => {
				let mut parts = vec![Part::Field(far.into())];
				parts.extend(field_parts(props));
				parts
			}
			(ScopeKind::Edge, Role::Anchor) => {
				if props.is_empty() {
					return Ok(Expr::Param(Param::new("parent")));
				}
				let mut parts = vec![Part::Start(Expr::Param(Param::new("parent")))];
				parts.extend(field_parts(props));
				parts
			}
			(ScopeKind::PostSplit, Role::Anchor) => {
				let mut parts = vec![Part::Field("__a".into())];
				parts.extend(field_parts(props));
				parts
			}
			(ScopeKind::PostSplit, Role::Edge) => {
				let mut parts = vec![Part::Field("__m".into())];
				parts.extend(field_parts(props));
				parts
			}
			(ScopeKind::PostSplit, Role::FarNode) => {
				let mut parts = vec![Part::Field("__m".into())];
				if !self.bindings.var_length {
					parts.push(Part::Field(far.into()));
				}
				if props.is_empty() {
					parts.push(Part::All);
				} else {
					parts.extend(field_parts(props));
				}
				parts
			}
			// The conjunct classifier only routes edge-scope-expressible
			// conjuncts into the lookup cond; reaching this arm is a
			// lowering bug, reported as an error rather than a panic.
			(ScopeKind::Anchor | ScopeKind::Edge, _) => {
				return Err(syntax_error!(
					"Internal error: variable is not addressable in this scope",
					@span
				));
			}
		};
		Ok(Expr::Idiom(Idiom(parts)))
	}
}

fn field_parts(props: &[&Ident]) -> Vec<Part> {
	props.iter().map(|p| Part::Field(p.name.clone().into())).collect()
}

/// Lowers a GQL expression in value position (projections, sort keys,
/// comparison operands): variable references resolve per the scope's
/// addressing table; comparisons and logical operators lower two-valued —
/// the §4 guards apply to predicate position only.
pub(super) async fn lower_value(
	stk: &mut Stk,
	expr: &GqlExpr,
	scope: &Scope<'_>,
) -> Result<Expr, SyntaxError> {
	match expr {
		GqlExpr::Literal(lit, _) => Ok(lower_literal(lit)),
		GqlExpr::Param {
			name,
			span,
		} => lower_param(name, *span),
		GqlExpr::Variable(ident) => {
			let role = scope.bindings.resolve(ident)?;
			scope.role_expr(role, &[], ident.span)
		}
		GqlExpr::Property(..) => lower_property(stk, expr, scope).await,
		GqlExpr::Unary {
			op,
			expr: operand,
			..
		} => {
			let operand = stk.run(|stk| lower_value(stk, operand, scope)).await?;
			let op = match op {
				UnaryOp::Not => PrefixOperator::Not,
				UnaryOp::Neg => PrefixOperator::Negate,
				UnaryOp::Plus => PrefixOperator::Positive,
			};
			Ok(Expr::Prefix {
				op,
				expr: Box::new(operand),
			})
		}
		GqlExpr::Binary {
			left,
			op,
			right,
			span,
		} => {
			let op = binary_op(*op, *span)?;
			let left = stk.run(|stk| lower_value(stk, left, scope)).await?;
			let right = stk.run(|stk| lower_value(stk, right, scope)).await?;
			Ok(Expr::Binary {
				left: Box::new(left),
				op,
				right: Box::new(right),
			})
		}
		GqlExpr::IsBool {
			..
		}
		| GqlExpr::IsNull {
			..
		} => lower_truth_test(stk, expr, false, scope).await,
		GqlExpr::FunctionCall {
			name,
			quantifier,
			star,
			span,
			..
		} => reject_function(name, *quantifier, *star, *span),
		GqlExpr::List(items, _) => {
			let mut exprs = Vec::with_capacity(items.len());
			for item in items {
				exprs.push(stk.run(|stk| lower_value(stk, item, scope)).await?);
			}
			Ok(Expr::Literal(Literal::Array(exprs)))
		}
		GqlExpr::Map(fields, _) => {
			let mut entries = Vec::with_capacity(fields.len());
			for (key, value) in fields {
				let value = stk.run(|stk| lower_value(stk, value, scope)).await?;
				entries.push(ObjectEntry {
					key: key.name.clone().into(),
					value,
				});
			}
			Ok(Expr::Literal(Literal::Object(entries)))
		}
	}
}

/// Lowers a GQL expression in predicate (`WHERE`) position with GQL's
/// three-valued semantics: rows are kept only when the predicate is TRUE.
/// `negated` is the pending NNF negation pushed down from enclosing `NOT`s.
pub(super) async fn lower_predicate(
	stk: &mut Stk,
	expr: &GqlExpr,
	negated: bool,
	scope: &Scope<'_>,
) -> Result<Expr, SyntaxError> {
	match expr {
		GqlExpr::Unary {
			op: UnaryOp::Not,
			expr: inner,
			..
		} => stk.run(|stk| lower_predicate(stk, inner, !negated, scope)).await,
		GqlExpr::Binary {
			op: op @ (BinaryOp::And | BinaryOp::Or),
			left,
			right,
			..
		} => {
			let lowered_left = stk.run(|stk| lower_predicate(stk, left, negated, scope)).await?;
			let lowered_right = stk.run(|stk| lower_predicate(stk, right, negated, scope)).await?;
			// De Morgan: a negated AND becomes an OR of the negated branches.
			let and = (*op == BinaryOp::And) != negated;
			Ok(Expr::Binary {
				left: Box::new(lowered_left),
				op: if and {
					BinaryOperator::And
				} else {
					BinaryOperator::Or
				},
				right: Box::new(lowered_right),
			})
		}
		GqlExpr::Binary {
			op: BinaryOp::Xor,
			span,
			..
		} => reject_xor(*span),
		GqlExpr::Binary {
			op:
				op @ (BinaryOp::Eq
				| BinaryOp::Neq
				| BinaryOp::Lt
				| BinaryOp::Lte
				| BinaryOp::Gt
				| BinaryOp::Gte),
			left,
			right,
			span,
		} => lower_comparison(stk, *op, negated, left, right, *span, scope).await,
		GqlExpr::IsBool {
			..
		}
		| GqlExpr::IsNull {
			..
		} => lower_truth_test(stk, expr, negated, scope).await,
		GqlExpr::Literal(GqlLiteral::Bool(b), _) => Ok(Expr::Literal(Literal::Bool(*b != negated))),
		// Any other expression in predicate position is tested against an
		// explicit boolean, so that a NULL/NONE value excludes the row:
		// `b.flag` → `b.flag = true`, `NOT b.flag` → `b.flag = false` (§4).
		other => {
			let value = stk.run(|stk| lower_value(stk, other, scope)).await?;
			Ok(equality(value, Literal::Bool(!negated), false))
		}
	}
}

/// Lowers a property-map entry on a pattern element into the
/// `<element>.key = value` equality conjunct (§3), with the §4 equality
/// guards. The element is addressed by role so that elements without a
/// variable still lower.
pub(super) async fn lower_prop_equality(
	stk: &mut Stk,
	role: Role,
	key: &Ident,
	value: &GqlExpr,
	scope: &Scope<'_>,
) -> Result<Expr, SyntaxError> {
	let left = scope.role_expr(role, &[key], key.span)?;
	// The property side is always nullable; guard only when the value side
	// is nullable too (§4: `x = <literal>` needs no guard).
	let guards = if nullable(value) {
		let mut atoms = vec![left.clone()];
		for atom in nullable_atoms(value) {
			let atom = stk.run(|stk| lower_value(stk, atom, scope)).await?;
			if !atoms.contains(&atom) {
				atoms.push(atom);
			}
		}
		guard_conjuncts(&atoms)
	} else {
		Vec::new()
	};
	let right = stk.run(|stk| lower_value(stk, value, scope)).await?;
	let comparison = Expr::Binary {
		left: Box::new(left),
		op: BinaryOperator::Equal,
		right: Box::new(right),
	};
	Ok(match and_chain(guards) {
		Some(guards) => Expr::Binary {
			left: Box::new(guards),
			op: BinaryOperator::And,
			right: Box::new(comparison),
		},
		None => comparison,
	})
}

/// Folds expressions into a left-associative `AND` chain.
pub(super) fn and_chain(exprs: impl IntoIterator<Item = Expr>) -> Option<Expr> {
	exprs.into_iter().reduce(|left, right| Expr::Binary {
		left: Box::new(left),
		op: BinaryOperator::And,
		right: Box::new(right),
	})
}

/// Lowers a comparison leaf with the §4 null guards. `negated` complements
/// the operator first (NNF), so the guards apply to the effective
/// comparison.
async fn lower_comparison(
	stk: &mut Stk,
	op: BinaryOp,
	negated: bool,
	left: &GqlExpr,
	right: &GqlExpr,
	span: Span,
	scope: &Scope<'_>,
) -> Result<Expr, SyntaxError> {
	let op = if negated {
		complement(op)
	} else {
		op
	};
	let guard_atoms: Vec<&GqlExpr> = match op {
		// Ordering comparisons: SurrealQL's total order sorts NULL/NONE
		// below numbers, so every nullable operand needs a guard (E8a/E8b).
		BinaryOp::Lt | BinaryOp::Lte | BinaryOp::Gt | BinaryOp::Gte => {
			let mut atoms = nullable_atoms(left);
			atoms.extend(nullable_atoms(right));
			atoms
		}
		// `=` deviates from GQL only when both sides can be null
		// (`NULL = NULL` is true in SurrealQL — E8c); a one-sided null
		// already compares unequal and excludes the row.
		BinaryOp::Eq if nullable(left) && nullable(right) => {
			let mut atoms = nullable_atoms(left);
			atoms.extend(nullable_atoms(right));
			atoms
		}
		// `<>` deviates whenever either side can be null (`NULL != 1` is
		// true in SurrealQL but UNKNOWN in GQL), so guard one-sided too.
		BinaryOp::Neq if nullable(left) || nullable(right) => {
			let mut atoms = nullable_atoms(left);
			atoms.extend(nullable_atoms(right));
			atoms
		}
		_ => Vec::new(),
	};
	// Lower and deduplicate the guarded atoms, preserving first-occurrence
	// order.
	let mut atoms: Vec<Expr> = Vec::new();
	for atom in guard_atoms {
		let atom = stk.run(|stk| lower_value(stk, atom, scope)).await?;
		if !atoms.contains(&atom) {
			atoms.push(atom);
		}
	}
	let lowered_left = stk.run(|stk| lower_value(stk, left, scope)).await?;
	let lowered_right = stk.run(|stk| lower_value(stk, right, scope)).await?;
	let comparison = Expr::Binary {
		left: Box::new(lowered_left),
		op: binary_op(op, span)?,
		right: Box::new(lowered_right),
	};
	Ok(match and_chain(guard_conjuncts(&atoms)) {
		Some(guards) => Expr::Binary {
			left: Box::new(guards),
			op: BinaryOperator::And,
			right: Box::new(comparison),
		},
		None => comparison,
	})
}

/// Builds the `atom != NONE AND atom != NULL` guard pair for each atom (§4).
fn guard_conjuncts(atoms: &[Expr]) -> Vec<Expr> {
	let mut out = Vec::with_capacity(atoms.len() * 2);
	for atom in atoms {
		out.push(Expr::Binary {
			left: Box::new(atom.clone()),
			op: BinaryOperator::NotEqual,
			right: Box::new(Expr::Literal(Literal::None)),
		});
		out.push(Expr::Binary {
			left: Box::new(atom.clone()),
			op: BinaryOperator::NotEqual,
			right: Box::new(Expr::Literal(Literal::Null)),
		});
	}
	out
}

/// Lowers `IS [NOT] NULL` and `IS [NOT] TRUE|FALSE|UNKNOWN` boolean tests
/// (§4). Truth tests are two-valued in GQL, so they need no guards;
/// `outer_negated` is the NNF negation pushed down from enclosing `NOT`s,
/// which negates the whole test.
async fn lower_truth_test(
	stk: &mut Stk,
	expr: &GqlExpr,
	outer_negated: bool,
	scope: &Scope<'_>,
) -> Result<Expr, SyntaxError> {
	match expr {
		GqlExpr::IsNull {
			expr: operand,
			negated,
			..
		} => {
			let value = stk.run(|stk| lower_value(stk, operand, scope)).await?;
			Ok(null_test(value, outer_negated != *negated))
		}
		GqlExpr::IsBool {
			expr: operand,
			value,
			negated,
			..
		} => {
			let operand = stk.run(|stk| lower_value(stk, operand, scope)).await?;
			let negated = outer_negated != *negated;
			match value {
				TruthValue::True => Ok(equality(operand, Literal::Bool(true), negated)),
				TruthValue::False => Ok(equality(operand, Literal::Bool(false), negated)),
				TruthValue::Unknown => Ok(null_test(operand, negated)),
			}
		}
		// Only called with `IsNull`/`IsBool` expressions.
		other => Err(syntax_error!("Internal error: not a truth test", @other.span())),
	}
}

/// `x IS NULL` → `x = NULL OR x = NONE`; negated, `x != NULL AND x != NONE`.
/// GQL cannot observe SurrealDB's NONE-vs-NULL distinction (§4).
fn null_test(value: Expr, negated: bool) -> Expr {
	let (cmp, combine) = if negated {
		(BinaryOperator::NotEqual, BinaryOperator::And)
	} else {
		(BinaryOperator::Equal, BinaryOperator::Or)
	};
	Expr::Binary {
		left: Box::new(Expr::Binary {
			left: Box::new(value.clone()),
			op: cmp.clone(),
			right: Box::new(Expr::Literal(Literal::Null)),
		}),
		op: combine,
		right: Box::new(Expr::Binary {
			left: Box::new(value),
			op: cmp,
			right: Box::new(Expr::Literal(Literal::None)),
		}),
	}
}

/// An (in)equality against a literal.
fn equality(left: Expr, literal: Literal, negated: bool) -> Expr {
	Expr::Binary {
		left: Box::new(left),
		op: if negated {
			BinaryOperator::NotEqual
		} else {
			BinaryOperator::Equal
		},
		right: Box::new(Expr::Literal(literal)),
	}
}

/// Collects the §4 guard atoms of a comparison operand: the property
/// accesses and parameters which can evaluate to `NULL`/`NONE`, reachable
/// through arithmetic, concatenation and sign operators. Literals,
/// containers, bound variables and boolean-valued sub-expressions are never
/// guarded.
fn nullable_atoms(expr: &GqlExpr) -> Vec<&GqlExpr> {
	let mut out = Vec::new();
	let mut stack = vec![expr];
	while let Some(e) = stack.pop() {
		match e {
			GqlExpr::Property(..)
			| GqlExpr::Param {
				..
			} => out.push(e),
			GqlExpr::Unary {
				op: UnaryOp::Neg | UnaryOp::Plus,
				expr,
				..
			} => stack.push(expr),
			GqlExpr::Binary {
				op: BinaryOp::Concat | BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div,
				left,
				right,
				..
			} => {
				stack.push(right);
				stack.push(left);
			}
			_ => {}
		}
	}
	out
}

/// Returns whether a comparison operand can evaluate to `NULL`/`NONE`.
fn nullable(expr: &GqlExpr) -> bool {
	let mut stack = vec![expr];
	while let Some(e) = stack.pop() {
		match e {
			GqlExpr::Property(..)
			| GqlExpr::Param {
				..
			}
			| GqlExpr::Literal(GqlLiteral::Null, _) => return true,
			GqlExpr::Unary {
				op: UnaryOp::Neg | UnaryOp::Plus,
				expr,
				..
			} => stack.push(expr),
			GqlExpr::Binary {
				op: BinaryOp::Concat | BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div,
				left,
				right,
				..
			} => {
				stack.push(right);
				stack.push(left);
			}
			_ => {}
		}
	}
	false
}

/// The complement of a comparison under NNF: `NOT (x < y)` ≡ `x >= y`.
fn complement(op: BinaryOp) -> BinaryOp {
	match op {
		BinaryOp::Eq => BinaryOp::Neq,
		BinaryOp::Neq => BinaryOp::Eq,
		BinaryOp::Lt => BinaryOp::Gte,
		BinaryOp::Lte => BinaryOp::Gt,
		BinaryOp::Gt => BinaryOp::Lte,
		BinaryOp::Gte => BinaryOp::Lt,
		other => other,
	}
}

/// Maps a GQL binary operator onto its SurrealQL counterpart. `XOR` has no
/// exactly-equivalent three-valued lowering and is rejected (§4, §7).
fn binary_op(op: BinaryOp, span: Span) -> Result<BinaryOperator, SyntaxError> {
	Ok(match op {
		BinaryOp::Or => BinaryOperator::Or,
		BinaryOp::Xor => return reject_xor(span),
		BinaryOp::And => BinaryOperator::And,
		BinaryOp::Eq => BinaryOperator::Equal,
		BinaryOp::Neq => BinaryOperator::NotEqual,
		BinaryOp::Lt => BinaryOperator::LessThan,
		BinaryOp::Lte => BinaryOperator::LessThanEqual,
		BinaryOp::Gt => BinaryOperator::MoreThan,
		BinaryOp::Gte => BinaryOperator::MoreThanEqual,
		// GQL `||` is string concatenation, which SurrealQL spells `+`.
		BinaryOp::Concat | BinaryOp::Add => BinaryOperator::Add,
		BinaryOp::Sub => BinaryOperator::Subtract,
		BinaryOp::Mul => BinaryOperator::Multiply,
		BinaryOp::Div => BinaryOperator::Divide,
	})
}

fn reject_xor<T>(span: Span) -> Result<T, SyntaxError> {
	bail!(
		"`XOR` is not supported yet",
		@span => "rewrite `a XOR b` as `(a OR b) AND NOT (a AND b)`"
	);
}

fn lower_param(name: &str, span: Span) -> Result<Expr, SyntaxError> {
	naming::validate_param_name(name, span)?;
	Ok(Expr::Param(Param::new(name)))
}

fn lower_literal(literal: &GqlLiteral) -> Expr {
	Expr::Literal(match literal {
		GqlLiteral::Null => Literal::Null,
		GqlLiteral::Bool(b) => Literal::Bool(*b),
		GqlLiteral::Integer(i) => Literal::Integer(*i),
		GqlLiteral::Float(f) => Literal::Float(*f),
		GqlLiteral::String(s) => Literal::String(s.clone().into()),
	})
}

/// Lowers a property access chain: a chain rooted at a pattern variable
/// resolves per the scope's addressing table; any other root lowers as a
/// value and the chain is appended as idiom fields.
async fn lower_property(
	stk: &mut Stk,
	expr: &GqlExpr,
	scope: &Scope<'_>,
) -> Result<Expr, SyntaxError> {
	let mut names: Vec<&Ident> = Vec::new();
	let mut base = expr;
	while let GqlExpr::Property(inner, name, _) = base {
		names.push(name);
		base = inner;
	}
	names.reverse();
	match base {
		GqlExpr::Variable(ident) => {
			let role = scope.bindings.resolve(ident)?;
			scope.role_expr(role, &names, ident.span)
		}
		other => {
			let start = stk.run(|stk| lower_value(stk, other, scope)).await?;
			let mut parts = match start {
				Expr::Idiom(idiom) => idiom.0,
				start => vec![Part::Start(start)],
			};
			parts.extend(names.iter().map(|name| Part::Field(name.name.clone().into())));
			Ok(Expr::Idiom(Idiom(parts)))
		}
	}
}

/// §5: the v1 function whitelist is empty — every call is rejected, with a
/// dedicated message for aggregates (including `count(*)` and
/// `count(DISTINCT …)` forms).
const AGGREGATE_FUNCTIONS: &[&str] = &[
	"avg",
	"collect_list",
	"count",
	"max",
	"min",
	"percentile_cont",
	"percentile_disc",
	"stddev_pop",
	"stddev_samp",
	"sum",
];

fn reject_function(
	name: &Ident,
	quantifier: Option<SetQuantifier>,
	star: Option<Span>,
	span: Span,
) -> Result<Expr, SyntaxError> {
	let lowered = name.name.to_ascii_lowercase();
	if star.is_some() || quantifier.is_some() || AGGREGATE_FUNCTIONS.contains(&lowered.as_str()) {
		bail!("Aggregate functions are not supported yet", @span);
	}
	bail!("The function `{}` is not supported yet", name.name, @span);
}
