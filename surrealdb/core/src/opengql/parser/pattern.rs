//! Graph pattern parsing: path patterns, node and edge patterns, label
//! expressions and quantifiers.
//!
//! From 16.4-16.11 (GQL.g4:803-1146). A linear path is grammatically just a
//! sequence of element patterns (`pathTerm : pathFactor+`); node-edge-node
//! alternation is enforced here semantically, with targeted errors.

use reblessive::Stk;

use crate::opengql::ast::{
	EdgeDirection, EdgePattern, ElementPredicate, Ident, LabelExpr, NodePattern, PathPattern,
	PathStep, Quantifier, QuantifierKind,
};
use crate::opengql::parser::mac::{enter_object_recursion, expected, unexpected};
use crate::opengql::parser::{ParseResult, Parser};
use crate::opengql::token::{Keyword, NumberKind, TokenKind, t};
use crate::syn::error::bail;

impl Parser<'_> {
	/// Parse a single path pattern: `pathVariableDeclaration?
	/// pathPatternPrefix? pathPatternExpression` (GQL.g4:834), where the
	/// expression must be a node pattern followed by zero or more edge-node
	/// steps.
	pub(super) async fn parse_path_pattern(&mut self, stk: &mut Stk) -> ParseResult<PathPattern> {
		// A path variable declaration is an identifier directly followed by
		// `=`; an identifier in pattern position can be nothing else.
		let path_var = if Self::token_can_be_ident(self.peek_kind()) && self.peek1().kind == t!("=")
		{
			let ident = self.parse_ident()?;
			self.pop_peek();
			Some(ident)
		} else {
			None
		};

		let token = self.peek();
		match token.kind {
			// `pathPatternPrefix` (GQL.g4:898-962): path modes and path
			// searches. WALK/TRAIL/SIMPLE/ACYCLIC are non-reserved words but
			// cannot start a path pattern expression, so they are
			// unambiguously a prefix here (a path variable was handled
			// above).
			t!("WALK")
			| t!("TRAIL")
			| t!("SIMPLE")
			| t!("ACYCLIC")
			| t!("ALL")
			| t!("ANY")
			| t!("SHORTEST") => {
				bail!(
					"Path pattern prefixes (path modes and path searches) are not supported yet",
					@token.span
				);
			}
			// A path pattern starting with an edge pattern has no source
			// node; reject it before node parsing produces a generic error.
			kind if edge_pattern_starts(kind) => {
				bail!(
					"Unexpected edge pattern, path patterns must start with a node pattern",
					@token.span
				);
			}
			kind if simplified_path_starts(kind) => {
				bail!(
					"Simplified path pattern expressions (`-/ … /->`) are not supported yet",
					@token.span
				);
			}
			_ => {}
		}

		let start = self.parse_node_pattern(stk).await?;
		let mut steps = Vec::new();
		loop {
			let token = self.peek();
			match token.kind {
				// Quantifiers are grammatically valid on any path factor but
				// only meaningful on edge patterns; give a targeted error.
				t!("*") | t!("+") | t!("?") | t!("{") => {
					bail!(
						"Quantifiers may only follow an edge pattern",
						@token.span
					);
				}
				// `(a)(b)` is grammatically a path term of two factors but
				// never semantically valid.
				t!("(") => {
					bail!(
						"Unexpected node pattern, expected an edge pattern between node patterns",
						@token.span
					);
				}
				kind if simplified_path_starts(kind) => {
					bail!(
						"Simplified path pattern expressions (`-/ … /->`) are not supported yet",
						@token.span
					);
				}
				_ => {}
			}
			let Some(edge) = self.parse_edge_pattern(stk).await? else {
				break;
			};
			let token = self.peek();
			if token.kind != t!("(") {
				unexpected!(self, token, "a node pattern after this edge pattern");
			}
			let node = self.parse_node_pattern(stk).await?;
			steps.push(PathStep {
				edge,
				node,
			});
		}

		Ok(PathPattern {
			path_var,
			start,
			steps,
		})
	}

	/// Parse a node pattern: `LEFT_PAREN elementPatternFiller RIGHT_PAREN`
	/// (GQL.g4:993).
	async fn parse_node_pattern(&mut self, stk: &mut Stk) -> ParseResult<NodePattern> {
		let token = self.peek();
		if token.kind != t!("(") {
			unexpected!(self, token, "a node pattern");
		}
		let open = self.pop_peek().span;

		// A `(` or an edge pattern directly inside the parentheses means
		// this is a `parenthesizedPathPatternExpression` (GQL.g4:1088), not
		// a node pattern.
		let next = self.peek();
		if next.kind == t!("(")
			|| edge_pattern_starts(next.kind)
			|| simplified_path_starts(next.kind)
		{
			bail!(
				"Parenthesized path pattern expressions are not supported yet",
				@next.span,
				@open => "expected this to start a node pattern"
			);
		}

		let (var, label, predicate) = self.parse_element_filler(stk).await?;

		// A `=` after the variable is a subpath variable declaration, which
		// also only occurs in parenthesized path pattern expressions.
		if self.peek_kind() == t!("=") {
			bail!(
				"Parenthesized path pattern expressions (subpath variables) are not supported yet",
				@self.peek().span
			);
		}

		self.expect_closing_delimiter(t!(")"), open)?;
		Ok(NodePattern {
			var,
			label,
			predicate,
			span: open.covers(self.last_span()),
		})
	}

	/// Parse an edge pattern, or return `None` when the next token does not
	/// start one. From `edgePattern` (GQL.g4:1035-1086): seven full forms
	/// wrapping an element pattern filler and seven abbreviated forms, with
	/// an optional postfix quantifier.
	async fn parse_edge_pattern(&mut self, stk: &mut Stk) -> ParseResult<Option<EdgePattern>> {
		let token = self.peek();
		// Abbreviated forms first: `<-` `~` `->` `<~` `~>` `<->` `-`.
		let abbreviated = match token.kind {
			t!("<-") => Some(EdgeDirection::Left),
			t!("~") => Some(EdgeDirection::Undirected),
			t!("->") => Some(EdgeDirection::Right),
			t!("<~") => Some(EdgeDirection::LeftOrUndirected),
			t!("~>") => Some(EdgeDirection::UndirectedOrRight),
			t!("<->") => Some(EdgeDirection::LeftOrRight),
			t!("-") => Some(EdgeDirection::Any),
			_ => None,
		};
		if let Some(direction) = abbreviated {
			self.pop_peek();
			let quantifier = self.parse_quantifier()?;
			return Ok(Some(EdgePattern {
				var: None,
				label: None,
				direction,
				predicate: None,
				quantifier,
				span: token.span.covers(self.last_span()),
			}));
		}

		// Full forms: the opening bracket token selects the possible
		// directions and the closing bracket token decides between them.
		let direction = match token.kind {
			t!("-[") => {
				self.pop_peek();
				let filler = self.parse_element_filler(stk).await?;
				let close = self.next();
				let direction = match close.kind {
					t!("]->") => EdgeDirection::Right,
					t!("]-") => EdgeDirection::Any,
					_ => unexpected!(self, close, "`]->` or `]-`"),
				};
				(filler, direction)
			}
			t!("<-[") => {
				self.pop_peek();
				let filler = self.parse_element_filler(stk).await?;
				let close = self.next();
				let direction = match close.kind {
					t!("]-") => EdgeDirection::Left,
					t!("]->") => EdgeDirection::LeftOrRight,
					_ => unexpected!(self, close, "`]-` or `]->`"),
				};
				(filler, direction)
			}
			t!("~[") => {
				self.pop_peek();
				let filler = self.parse_element_filler(stk).await?;
				let close = self.next();
				let direction = match close.kind {
					t!("]~") => EdgeDirection::Undirected,
					t!("]~>") => EdgeDirection::UndirectedOrRight,
					_ => unexpected!(self, close, "`]~` or `]~>`"),
				};
				(filler, direction)
			}
			t!("<~[") => {
				self.pop_peek();
				let filler = self.parse_element_filler(stk).await?;
				let close = self.next();
				let direction = match close.kind {
					t!("]~") => EdgeDirection::LeftOrUndirected,
					_ => unexpected!(self, close, "`]~`"),
				};
				(filler, direction)
			}
			_ => return Ok(None),
		};
		let ((var, label, predicate), direction) = direction;
		let quantifier = self.parse_quantifier()?;
		Ok(Some(EdgePattern {
			var,
			label,
			direction,
			predicate,
			quantifier,
			span: token.span.covers(self.last_span()),
		}))
	}

	/// Parse an element pattern filler: `elementVariableDeclaration?
	/// isLabelExpression? elementPatternPredicate?` (GQL.g4:997), shared
	/// between node and edge patterns. All three parts are optional; the
	/// predicate is either an inline `WHERE` or a property map, never both.
	async fn parse_element_filler(
		&mut self,
		stk: &mut Stk,
	) -> ParseResult<(Option<Ident>, Option<LabelExpr>, Option<ElementPredicate>)> {
		let token = self.peek();
		let var = match token.kind {
			kind if Self::token_can_be_ident(kind) => Some(self.parse_ident()?),
			// A reserved word cannot be a variable; everything else that may
			// legitimately appear here is handled below.
			TokenKind::Keyword(keyword) if !matches!(keyword, Keyword::Is | Keyword::Where) => {
				bail!(
					"`{}` is a reserved word and cannot be used as a variable name",
					self.span_str(token.span),
					@token.span => "use a `\"…\"` or `` `…` `` delimited identifier instead"
				);
			}
			_ => None,
		};

		// Labels are introduced by `:` or the keyword `IS` (`isOrColon`,
		// GQL.g4:1024).
		let label = if self.eat(t!(":")) || self.eat(t!("IS")) {
			Some(self.parse_label_expr(stk).await?)
		} else {
			None
		};

		let predicate = match self.peek_kind() {
			t!("WHERE") => {
				self.pop_peek();
				let expr = stk.run(|stk| self.parse_expr(stk)).await?;
				Some(ElementPredicate::Where(expr))
			}
			t!("{") => {
				let open = self.pop_peek().span;
				let mut props = Vec::new();
				loop {
					let key = self.parse_ident()?;
					expected!(self, t!(":"));
					let value = stk.run(|stk| self.parse_expr(stk)).await?;
					props.push((key, value));
					if !self.eat(t!(",")) {
						break;
					}
				}
				self.expect_closing_delimiter(t!("}"), open)?;
				Some(ElementPredicate::Props(props))
			}
			_ => None,
		};

		// `elementPatternPredicate` (GQL.g4:1009) is a single alternative: a
		// filler has a WHERE clause or a property map, never both.
		let token = self.peek();
		if predicate.is_some() && matches!(token.kind, t!("WHERE") | t!("{")) {
			bail!(
				"An element pattern may have either a WHERE clause or a property map, not both",
				@token.span
			);
		}

		Ok((var, label, predicate))
	}

	/// Parse an optional postfix graph pattern quantifier: `*`, `+`, `?`,
	/// `{n}`, `{n,m}`, `{n,}`, `{,m}` or `{,}` (GQL.g4:1125-1146).
	fn parse_quantifier(&mut self) -> ParseResult<Option<Quantifier>> {
		let token = self.peek();
		let kind = match token.kind {
			t!("*") => {
				self.pop_peek();
				QuantifierKind::Star
			}
			t!("+") => {
				self.pop_peek();
				QuantifierKind::Plus
			}
			t!("?") => {
				self.pop_peek();
				QuantifierKind::Question
			}
			t!("{") => {
				let open = self.pop_peek().span;
				let lower = match self.peek_kind() {
					t!(",") => None,
					// A number token may still be rejected as a bound (a
					// float, a suffix); the bound parser reports those.
					TokenKind::Number {
						..
					} => Some(self.parse_quantifier_bound()?),
					_ => {
						let token = self.peek();
						unexpected!(self, token, "an unsigned integer or `,`");
					}
				};
				let kind = if self.eat(t!(",")) {
					let upper = if self.peek_kind() == t!("}") {
						None
					} else {
						Some(self.parse_quantifier_bound()?)
					};
					QuantifierKind::Range(lower, upper)
				} else {
					// Without a comma this is a `fixedQuantifier`, which
					// requires the bound.
					match lower {
						Some(n) => QuantifierKind::Fixed(n),
						None => {
							let token = self.peek();
							unexpected!(self, token, "an unsigned integer or `,`");
						}
					}
				};
				self.expect_closing_delimiter(t!("}"), open)?;
				kind
			}
			_ => return Ok(None),
		};
		Ok(Some(Quantifier {
			kind,
			span: token.span.covers(self.last_span()),
		}))
	}

	/// Parse an unsigned integer quantifier bound.
	fn parse_quantifier_bound(&mut self) -> ParseResult<u32> {
		let token = self.next();
		match token.kind {
			TokenKind::Number {
				kind:
					kind @ (NumberKind::Integer
					| NumberKind::Hex
					| NumberKind::Octal
					| NumberKind::Binary),
				suffix: None,
			} => self.parse_u32_token(token, kind),
			_ => unexpected!(self, token, "an unsigned integer"),
		}
	}

	/// Parse a label expression (GQL.g4:1102-1109), with precedence
	/// `!` > `&` > `|`.
	async fn parse_label_expr(&mut self, stk: &mut Stk) -> ParseResult<LabelExpr> {
		let mut left = self.parse_label_conjunction(stk).await?;
		while self.eat(t!("|")) {
			let right = self.parse_label_conjunction(stk).await?;
			let span = left.span().covers(right.span());
			left = LabelExpr::Disjunction(Box::new(left), Box::new(right), span);
		}
		Ok(left)
	}

	async fn parse_label_conjunction(&mut self, stk: &mut Stk) -> ParseResult<LabelExpr> {
		let mut left = stk.run(|stk| self.parse_label_negation(stk)).await?;
		while self.eat(t!("&")) {
			let right = stk.run(|stk| self.parse_label_negation(stk)).await?;
			let span = left.span().covers(right.span());
			left = LabelExpr::Conjunction(Box::new(left), Box::new(right), span);
		}
		Ok(left)
	}

	async fn parse_label_negation(&mut self, stk: &mut Stk) -> ParseResult<LabelExpr> {
		let token = self.peek();
		if token.kind == t!("!") {
			self.pop_peek();
			let inner = stk.run(|stk| self.parse_label_negation(stk)).await?;
			let span = token.span.covers(inner.span());
			return Ok(LabelExpr::Negation(Box::new(inner), span));
		}
		self.parse_label_primary(stk).await
	}

	async fn parse_label_primary(&mut self, stk: &mut Stk) -> ParseResult<LabelExpr> {
		let token = self.peek();
		match token.kind {
			t!("%") => {
				self.pop_peek();
				Ok(LabelExpr::Wildcard(token.span))
			}
			t!("(") => {
				enter_object_recursion!(this = self => {
					let open = this.pop_peek().span;
					let inner = stk.run(|stk| this.parse_label_expr(stk)).await?;
					this.expect_closing_delimiter(t!(")"), open)?;
					Ok(inner)
				})
			}
			kind if Self::token_can_be_ident(kind) => Ok(LabelExpr::Name(self.parse_ident()?)),
			TokenKind::Keyword(_) => {
				// Reserved keyword: produce the reserved word error.
				self.parse_ident().map(LabelExpr::Name)
			}
			_ => unexpected!(self, token, "a label expression"),
		}
	}
}

/// Returns whether the token kind starts (or is) an edge pattern: a compound
/// bracket token or an abbreviated edge form.
fn edge_pattern_starts(kind: TokenKind) -> bool {
	matches!(
		kind,
		t!("-[")
			| t!("<-[")
			| t!("~[")
			| t!("<~[")
			| t!("<-")
			| t!("~") | t!("->")
			| t!("<~")
			| t!("~>")
			| t!("<->")
			| t!("-")
	)
}

/// Returns whether the token kind starts a simplified path pattern
/// expression (16.12, the `-/ … /->` slash forms).
fn simplified_path_starts(kind: TokenKind) -> bool {
	matches!(kind, t!("-/") | t!("<-/") | t!("~/") | t!("<~/"))
}
