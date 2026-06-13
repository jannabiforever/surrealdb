//! Lowering of the GQL AST onto the SurrealQL surface AST.
//!
//! Implements the normative design in `doc/opengql/LOWERING.md`: a parsed
//! [`GqlQuery`] is mapped directly onto [`crate::sql`] values — no SurrealQL
//! text is ever generated or re-parsed. The engine behaviors the
//! construction relies on are pinned by
//! `language-tests/tests/opengql/lowering_substrate.surql` (E1–E8).
//!
//! Layout: [`pattern`] analyzes and scaffolds the binding shape (§2, §3,
//! §6), [`expr`] lowers expressions per scope with the three-valued-logic
//! guards (§2.2, §4), [`naming`] owns the column-name and reserved-name
//! rules (§5), and this module dispatches and assembles the final
//! [`Ast`].

mod expr;
mod naming;
mod pattern;

#[cfg(test)]
mod test;

use reblessive::{Stack, Stk};

use self::expr::{Scope, ScopeKind};
use self::pattern::Placement;
use crate::opengql::ast::{
	GqlExpr, GqlLiteral, GqlQuery, GqlStatement, MatchQuery, OrderItem, ReturnClause, ReturnItems,
	SetQuantifier,
};
use crate::sql::field::Selector;
use crate::sql::order::{OrderList, Ordering};
use crate::sql::{
	Ast, Expr, Field, Fields, Group, Groups, Idiom, Limit, Literal, Order, Param, Start,
};
use crate::syn::error::{SyntaxError, bail, syntax_error};

/// Lowers a parsed GQL query onto the SurrealQL surface AST.
///
/// Runs on a [`reblessive`] stack: the GQL AST contains arbitrarily deep
/// expression chains which the machine stack must not recurse over.
pub(super) fn lower(query: GqlQuery) -> Result<Ast, SyntaxError> {
	let GqlQuery {
		stmt: GqlStatement::Match(query),
	} = query;
	let mut stack = Stack::new();
	stack.enter(|stk| lower_match_query(stk, &query)).finish()
}

async fn lower_match_query(stk: &mut Stk, query: &MatchQuery) -> Result<Ast, SyntaxError> {
	let clause = match query.matches.as_slice() {
		[clause] => clause,
		[] => {
			bail!(
				"A query without a MATCH clause is not supported yet",
				@query.ret.span => "start the query with a MATCH clause"
			);
		}
		[_, second, ..] => {
			bail!("Multiple MATCH clauses are not supported yet", @second.span);
		}
	};
	if clause.optional {
		bail!("OPTIONAL MATCH is not supported yet", @clause.span);
	}

	let shape = pattern::analyze(clause)?;
	let bindings = &shape.bindings;
	let Some(path) = clause.patterns.first() else {
		// `analyze` validated that exactly one pattern exists.
		return Err(syntax_error!(
			"Internal error: MATCH clause without a pattern",
			@clause.span
		));
	};

	// Predicate placement (§3): merge, NNF-split, classify and lower each
	// conjunct in the scope of its placement.
	let conjuncts = pattern::collect_conjuncts(clause.where_clause.as_ref(), path);
	let mut anchor_preds = Vec::new();
	let mut edge_preds = Vec::new();
	let mut post_preds = Vec::new();
	for conjunct in &conjuncts {
		let (kind, slot) = match pattern::classify(conjunct, bindings)? {
			Placement::Anchor => (ScopeKind::Anchor, &mut anchor_preds),
			Placement::Edge => (ScopeKind::Edge, &mut edge_preds),
			Placement::PostSplit => (ScopeKind::PostSplit, &mut post_preds),
		};
		let scope = Scope {
			kind,
			bindings,
		};
		slot.push(pattern::lower_conjunct(stk, conjunct, &scope).await?);
	}
	let anchor_cond = expr::and_chain(anchor_preds);
	let edge_cond =
		expr::and_chain(edge_preds.into_iter().chain(pattern::far_label_filter(&shape)));
	let post_cond = expr::and_chain(post_preds);

	let mut select = pattern::build_frame(&shape, anchor_cond, edge_cond, post_cond);

	// Projections, DISTINCT, ORDER BY and paging apply to the outermost
	// layer, in post-split scope — which for the degenerate no-edge shape
	// is the anchor scope itself (§2.1, §5).
	let scope = Scope {
		kind: if shape.hop.is_some() {
			ScopeKind::PostSplit
		} else {
			ScopeKind::Anchor
		},
		bindings,
	};
	let columns = lower_return_items(stk, &query.ret, &scope).await?;
	let distinct = matches!(query.ret.quantifier, Some(SetQuantifier::Distinct));
	if !query.ret.order_by.is_empty() {
		let mut orders = Vec::with_capacity(query.ret.order_by.len());
		for item in &query.ret.order_by {
			orders.push(lower_order_item(stk, item, &columns, &scope).await?);
		}
		select.order = Some(Ordering::Order(OrderList(orders)));
	}
	if distinct {
		// `RETURN DISTINCT` → GROUP BY all projected aliases (§5, E6).
		select.group =
			Some(Groups(columns.iter().map(|c| Group(Idiom::field(c.name.clone()))).collect()));
	}
	select.fields = Fields::Select(
		columns
			.into_iter()
			.map(|c| {
				Field::Single(Selector {
					expr: c.expr,
					alias: Some(Idiom::field(c.name)),
				})
			})
			.collect(),
	);
	if let Some(skip) = &query.ret.skip {
		select.start = Some(Start(lower_count(skip)?));
	}
	if let Some(limit) = &query.ret.limit {
		select.limit = Some(Limit(lower_count(limit)?));
	}

	Ok(Ast::single_expr(Expr::Select(Box::new(select))))
}

/// A projected column: its name (the row object key) and the lowered value
/// expression.
struct Column {
	name: String,
	expr: Expr,
}

/// Lowers the RETURN items into named columns (§5): explicit aliases win,
/// unaliased items are named by their verbatim source text, `RETURN *`
/// expands to the named pattern variables in alphabetical order, and
/// duplicate column names are rejected.
async fn lower_return_items(
	stk: &mut Stk,
	ret: &ReturnClause,
	scope: &Scope<'_>,
) -> Result<Vec<Column>, SyntaxError> {
	let mut columns: Vec<Column> = Vec::new();
	match &ret.items {
		ReturnItems::Star => {
			let mut vars = scope.bindings.vars.clone();
			vars.sort_by(|a, b| a.0.cmp(&b.0));
			if vars.is_empty() {
				bail!(
					"RETURN * requires at least one named pattern variable",
					@ret.span => "name a pattern element or list the return items explicitly"
				);
			}
			for (name, role) in vars {
				let expr = scope.role_expr(role, &[], ret.span)?;
				columns.push(Column {
					name,
					expr,
				});
			}
		}
		ReturnItems::Items(items) => {
			for item in items {
				let (name, name_span) = naming::column_name(item)?;
				if columns.iter().any(|c| c.name == name) {
					bail!(
						"Duplicate column name `{name}`",
						@name_span => "use `AS` to give the items distinct column names"
					);
				}
				let expr = expr::lower_value(stk, &item.expr, scope).await?;
				columns.push(Column {
					name,
					expr,
				});
			}
		}
	}
	Ok(columns)
}

/// Lowers an ORDER BY item (§5). Sort keys must name a RETURN column or
/// lower to the same expression as one; the row then sorts on that column
/// (alias resolution is engine-side, pinned for dotted names by E5/E6).
async fn lower_order_item(
	stk: &mut Stk,
	item: &OrderItem,
	columns: &[Column],
	scope: &Scope<'_>,
) -> Result<Order, SyntaxError> {
	if item.nulls_first.is_some() {
		bail!("`NULLS FIRST`/`NULLS LAST` ordering is not supported yet", @item.span);
	}
	let value = order_key(stk, item, columns, scope).await?;
	Ok(Order {
		value,
		collate: false,
		numeric: false,
		direction: item.ascending.unwrap_or(true),
	})
}

async fn order_key(
	stk: &mut Stk,
	item: &OrderItem,
	columns: &[Column],
	scope: &Scope<'_>,
) -> Result<Idiom, SyntaxError> {
	// A sort key matching a RETURN column by name: its alias or the
	// verbatim text of an unaliased item.
	if let Some(name) = order_key_name(&item.expr)
		&& columns.iter().any(|c| c.name == name)
	{
		return Ok(Idiom::field(name));
	}
	let lowered = expr::lower_value(stk, &item.expr, scope).await?;
	// A sort key lowering to the same expression as a RETURN item sorts on
	// that item's column.
	if let Some(column) = columns.iter().find(|c| c.expr == lowered) {
		return Ok(Idiom::field(column.name.clone()));
	}
	// Any other sort key is rejected: the legacy engine sorts the projected
	// output rows (a non-column key silently no-op sorts) while the
	// streaming engine resolves source fields (sorting correctly), so only
	// column-matching keys behave identically under every planner strategy
	// — the same invariant `syn` enforces for plain SELECT statements.
	bail!(
		"ORDER BY may only reference RETURN items",
		@item.span => "return the sort expression under an alias and order by the alias"
	);
}

/// The dotted name of a sort key that is a plain variable or property
/// chain, used to match RETURN columns by name.
fn order_key_name(expr: &GqlExpr) -> Option<String> {
	let mut names: Vec<&str> = Vec::new();
	let mut base = expr;
	while let GqlExpr::Property(inner, name, _) = base {
		names.push(&name.name);
		base = inner;
	}
	let GqlExpr::Variable(var) = base else {
		return None;
	};
	names.push(&var.name);
	names.reverse();
	Some(names.join("."))
}

/// Lowers a SKIP/LIMIT count: an unsigned integer literal or a parameter
/// (§5). The parser only produces these two forms.
fn lower_count(expr: &GqlExpr) -> Result<Expr, SyntaxError> {
	match expr {
		GqlExpr::Literal(GqlLiteral::Integer(i), _) => Ok(Expr::Literal(Literal::Integer(*i))),
		GqlExpr::Param {
			name,
			span,
		} => {
			naming::validate_param_name(name, *span)?;
			Ok(Expr::Param(Param::new(name.as_str())))
		}
		other => Err(syntax_error!(
			"Expected an unsigned integer or a parameter",
			@other.span()
		)),
	}
}
