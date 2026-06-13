//! Statement level parsing: the linear query statement, `MATCH` clauses and
//! the `RETURN` clause with its trailing `ORDER BY`/`OFFSET`/`LIMIT` page
//! statement.
//!
//! From `ambientLinearQueryStatement` (GQL.g4:554), `matchStatement` (14.4,
//! GQL.g4:578-599), `primitiveResultStatement`/`returnStatement` (14.10-14.11,
//! GQL.g4:660-685) and `orderByAndPageStatement` (14.9, GQL.g4:652). All
//! constructs which share grammar real estate with this subset but are not
//! supported are recognised and rejected with precise spans.

use reblessive::Stk;

use crate::opengql::ast::{
	GqlExpr, GqlLiteral, GqlStatement, MatchClause, MatchQuery, OrderItem, ReturnClause,
	ReturnItem, ReturnItems, SetQuantifier,
};
use crate::opengql::parser::mac::{expected, unexpected};
use crate::opengql::parser::{ParseResult, Parser};
use crate::opengql::token::{NumberKind, Span, TokenKind, t};
use crate::syn::error::bail;

impl Parser<'_> {
	/// Parse a top level statement: zero or more `MATCH` clauses followed by
	/// a `RETURN` clause.
	pub(super) async fn parse_statement(&mut self, stk: &mut Stk) -> ParseResult<GqlStatement> {
		let mut matches = Vec::new();
		loop {
			let token = self.peek();
			match token.kind {
				t!("MATCH") => {
					self.pop_peek();
					let clause = self.parse_match_clause(stk, false, token.span).await?;
					matches.push(clause);
				}
				t!("OPTIONAL") => {
					self.pop_peek();
					let next = self.peek();
					match next.kind {
						t!("MATCH") => {
							self.pop_peek();
							let clause = self.parse_match_clause(stk, true, token.span).await?;
							matches.push(clause);
						}
						// `optionalOperand` also allows `{`/`(` delimited
						// match statement blocks (GQL.g4:590-594).
						t!("{") | t!("(") => {
							bail!(
								"OPTIONAL MATCH blocks are not supported yet",
								@token.span.covers(next.span) => "use a plain `OPTIONAL MATCH` clause"
							);
						}
						_ => unexpected!(self, next, "`MATCH`"),
					}
				}
				t!("RETURN") => break,
				t!("FINISH") => {
					bail!(
						"FINISH statements are not supported yet",
						@token.span => "queries must end with a RETURN clause"
					);
				}
				t!("LET") | t!("FOR") | t!("FILTER") | t!("USE") | t!("SELECT") | t!("CALL") => {
					bail!(
						"`{}` statements are not supported yet",
						token.kind,
						@token.span
					);
				}
				t!("ORDER") | t!("OFFSET") | t!("SKIP") | t!("LIMIT") => {
					bail!(
						"Standalone `{}` statements between MATCH clauses are not supported yet",
						token.kind,
						@token.span => "ORDER BY, OFFSET/SKIP and LIMIT may only follow the RETURN clause"
					);
				}
				t!("INSERT")
				| t!("CREATE")
				| t!("SET")
				| t!("REMOVE")
				| t!("DELETE")
				| t!("DETACH")
				| t!("NODETACH")
				| t!("DROP") => {
					bail!(
						"GQL write statements are not supported in this version (read-only)",
						@token.span
					);
				}
				_ => unexpected!(self, token, "a MATCH or RETURN statement"),
			}
		}
		let ret = self.parse_return_clause(stk).await?;
		self.check_trailing_clauses()?;
		Ok(GqlStatement::Match(MatchQuery {
			matches,
			ret,
		}))
	}

	/// Parse a `MATCH` clause graph pattern: `matchMode? pathPatternList
	/// keepClause? graphPatternWhereClause?` (GQL.g4:803). The `MATCH` (and
	/// `OPTIONAL`) keywords must already be consumed; `start` is the span of
	/// the first of them.
	async fn parse_match_clause(
		&mut self,
		stk: &mut Stk,
		optional: bool,
		start: Span,
	) -> ParseResult<MatchClause> {
		// `REPEATABLE`/`DIFFERENT` are non-reserved words, so they are only a
		// match mode when followed by their element/edge synonym; otherwise
		// they can begin a path pattern as a path variable.
		let token = self.peek();
		if let t!("REPEATABLE") = token.kind
			&& matches!(self.peek1().kind, t!("ELEMENT") | t!("ELEMENTS"))
		{
			bail!(
				"Match modes (`REPEATABLE ELEMENTS`) are not supported yet",
				@token.span.covers(self.peek1().span)
			);
		}
		if let t!("DIFFERENT") = token.kind
			&& matches!(
				self.peek1().kind,
				t!("EDGE") | t!("EDGES") | t!("RELATIONSHIP") | t!("RELATIONSHIPS")
			) {
			bail!(
				"Match modes (`DIFFERENT EDGES`) are not supported yet",
				@token.span.covers(self.peek1().span)
			);
		}

		let mut patterns = Vec::new();
		loop {
			let pattern = self.parse_path_pattern(stk).await?;
			patterns.push(pattern);
			let token = self.peek();
			match token.kind {
				t!(",") => {
					self.pop_peek();
				}
				// `pathTerm |+| pathTerm` and `pathTerm | pathTerm`
				// alternations (GQL.g4:966-970).
				t!("|+|") => {
					bail!(
						"Multiset alternation (`|+|`) between path patterns is not supported yet",
						@token.span
					);
				}
				t!("|") => {
					bail!(
						"Pattern unions (`|`) between path patterns are not supported yet",
						@token.span
					);
				}
				_ => break,
			}
		}

		let token = self.peek();
		if let t!("KEEP") = token.kind {
			bail!("KEEP clauses are not supported yet", @token.span);
		}

		let where_clause = if self.eat(t!("WHERE")) {
			Some(stk.run(|stk| self.parse_expr(stk)).await?)
		} else {
			None
		};

		// `graphPatternYieldClause` (GQL.g4:597).
		let token = self.peek();
		if let t!("YIELD") = token.kind {
			bail!("YIELD clauses are not supported yet", @token.span);
		}

		Ok(MatchClause {
			optional,
			patterns,
			where_clause,
			span: start.covers(self.last_span()),
		})
	}

	/// Parse the `RETURN` clause and its optional trailing `ORDER BY`,
	/// `OFFSET`/`SKIP` and `LIMIT` clauses, which must appear in that fixed
	/// order.
	async fn parse_return_clause(&mut self, stk: &mut Stk) -> ParseResult<ReturnClause> {
		let start = expected!(self, t!("RETURN")).span;

		let quantifier = if self.eat(t!("DISTINCT")) {
			Some(SetQuantifier::Distinct)
		} else if self.eat(t!("ALL")) {
			Some(SetQuantifier::All)
		} else {
			None
		};

		let items = if self.eat(t!("*")) {
			ReturnItems::Star
		} else {
			let mut items = Vec::new();
			loop {
				items.push(self.parse_return_item(stk).await?);
				if !self.eat(t!(",")) {
					break;
				}
			}
			ReturnItems::Items(items)
		};

		// An attached `GROUP BY` (`groupByClause`, GQL.g4:1313) parses but is
		// not supported.
		let token = self.peek();
		if let t!("GROUP") = token.kind {
			self.pop_peek();
			let mut span = token.span;
			if let t!("BY") = self.peek_kind() {
				span = span.covers(self.pop_peek().span);
			}
			bail!("GROUP BY is not supported yet", @span);
		}

		let mut order_by = Vec::new();
		if self.eat(t!("ORDER")) {
			expected!(self, t!("BY"));
			loop {
				order_by.push(self.parse_order_item(stk).await?);
				if !self.eat(t!(",")) {
					break;
				}
			}
		}

		// `OFFSET` and `SKIP` are synonyms (`offsetSynonym`, GQL.g4:1374).
		let skip = if self.eat(t!("OFFSET")) || self.eat(t!("SKIP")) {
			Some(self.parse_count_specification()?)
		} else {
			None
		};

		let limit = if self.eat(t!("LIMIT")) {
			Some(self.parse_count_specification()?)
		} else {
			None
		};

		Ok(ReturnClause {
			quantifier,
			items,
			order_by,
			skip,
			limit,
			span: start.covers(self.last_span()),
		})
	}

	/// Parse a single `RETURN` item: an expression with an optional `AS`
	/// alias, capturing the verbatim source text of the expression for use as
	/// the default column name.
	async fn parse_return_item(&mut self, stk: &mut Stk) -> ParseResult<ReturnItem> {
		let start = self.peek().span;
		let expr = stk.run(|stk| self.parse_expr(stk)).await?;
		let text_span = start.covers(self.last_span());
		let text = self.span_str(text_span).to_owned();
		let alias = if self.eat(t!("AS")) {
			Some(self.parse_ident()?)
		} else {
			None
		};
		Ok(ReturnItem {
			expr,
			alias,
			text,
		})
	}

	/// Parse a single `ORDER BY` sort specification: `sortKey
	/// orderingSpecification? nullOrdering?` (GQL.g4:1341), where the sort
	/// key is a full value expression.
	async fn parse_order_item(&mut self, stk: &mut Stk) -> ParseResult<OrderItem> {
		let start = self.peek().span;
		let expr = stk.run(|stk| self.parse_expr(stk)).await?;
		let ascending = if self.eat(t!("ASC")) || self.eat(t!("ASCENDING")) {
			Some(true)
		} else if self.eat(t!("DESC")) || self.eat(t!("DESCENDING")) {
			Some(false)
		} else {
			None
		};
		let nulls_first = if self.eat(t!("NULLS")) {
			let token = self.next();
			match token.kind {
				t!("FIRST") => Some(true),
				t!("LAST") => Some(false),
				_ => unexpected!(self, token, "`FIRST` or `LAST`"),
			}
		} else {
			None
		};
		Ok(OrderItem {
			expr,
			ascending,
			nulls_first,
			span: start.covers(self.last_span()),
		})
	}

	/// Parse a `LIMIT`/`OFFSET` count: an unsigned integer or a parameter
	/// (`nonNegativeIntegerSpecification`, GQL.g4:2268).
	fn parse_count_specification(&mut self) -> ParseResult<GqlExpr> {
		let token = self.next();
		match token.kind {
			TokenKind::Number {
				kind:
					kind @ (NumberKind::Integer
					| NumberKind::Hex
					| NumberKind::Octal
					| NumberKind::Binary),
				suffix: None,
			} => {
				let value = self.parse_integer_token(token, kind)?;
				Ok(GqlExpr::Literal(GqlLiteral::Integer(value), token.span))
			}
			TokenKind::Parameter => {
				let name = self.parse_parameter_name(token)?;
				Ok(GqlExpr::Param {
					name,
					span: token.span,
				})
			}
			TokenKind::SubstitutedParameter => {
				bail!(
					"Substituted parameters (`$$name`) are not supported yet",
					@token.span => "use a general `$name` parameter"
				);
			}
			_ => unexpected!(self, token, "an unsigned integer or a parameter"),
		}
	}

	/// Rejects out-of-order, duplicated or composite trailing clauses with a
	/// targeted error before the generic end-of-query check runs.
	fn check_trailing_clauses(&mut self) -> ParseResult<()> {
		let token = self.peek();
		match token.kind {
			t!("ORDER") | t!("OFFSET") | t!("SKIP") | t!("LIMIT") => {
				bail!(
					"Unexpected `{}` clause",
					token.kind,
					@token.span => "ORDER BY, OFFSET/SKIP and LIMIT may appear at most once each, in that order"
				);
			}
			t!("UNION") | t!("EXCEPT") | t!("INTERSECT") | t!("OTHERWISE") => {
				bail!(
					"Composite queries (`UNION`, `EXCEPT`, `INTERSECT`, `OTHERWISE`) are not supported yet",
					@token.span
				);
			}
			t!("GROUP") => {
				bail!(
					"GROUP BY is not supported yet",
					@token.span => "GROUP BY must directly follow the RETURN items"
				);
			}
			_ => Ok(()),
		}
	}
}
