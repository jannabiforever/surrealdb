/// A shorthand for GQL token kinds.
macro_rules! t {
	("invalid") => {
		$crate::opengql::token::TokenKind::Invalid
	};
	("eof") => {
		$crate::opengql::token::TokenKind::Eof
	};

	("(") => {
		$crate::opengql::token::TokenKind::OpenParen
	};
	(")") => {
		$crate::opengql::token::TokenKind::CloseParen
	};
	("[") => {
		$crate::opengql::token::TokenKind::OpenBracket
	};
	("]") => {
		$crate::opengql::token::TokenKind::CloseBracket
	};
	("{") => {
		$crate::opengql::token::TokenKind::OpenBrace
	};
	("}") => {
		$crate::opengql::token::TokenKind::CloseBrace
	};

	("'") => {
		$crate::opengql::token::TokenKind::SingleQuoted {
			no_escape: false,
		}
	};
	("@'") => {
		$crate::opengql::token::TokenKind::SingleQuoted {
			no_escape: true,
		}
	};
	("\"") => {
		$crate::opengql::token::TokenKind::DoubleQuoted {
			no_escape: false,
		}
	};
	("@\"") => {
		$crate::opengql::token::TokenKind::DoubleQuoted {
			no_escape: true,
		}
	};
	("`") => {
		$crate::opengql::token::TokenKind::AccentQuoted {
			no_escape: false,
		}
	};
	("@`") => {
		$crate::opengql::token::TokenKind::AccentQuoted {
			no_escape: true,
		}
	};

	("$param") => {
		$crate::opengql::token::TokenKind::Parameter
	};
	("$$param") => {
		$crate::opengql::token::TokenKind::SubstitutedParameter
	};

	("|+|") => {
		$crate::opengql::token::TokenKind::MultisetAlternation
	};
	("]->") => {
		$crate::opengql::token::TokenKind::BracketRightArrow
	};
	("]~>") => {
		$crate::opengql::token::TokenKind::BracketTildeRightArrow
	};
	("||") => {
		$crate::opengql::token::TokenKind::Concat
	};
	("::") => {
		$crate::opengql::token::TokenKind::DoubleColon
	};
	("..") => {
		$crate::opengql::token::TokenKind::DoublePeriod
	};
	(">=") => {
		$crate::opengql::token::TokenKind::Gte
	};
	("<-") => {
		$crate::opengql::token::TokenKind::LeftArrow
	};
	("<~") => {
		$crate::opengql::token::TokenKind::LeftArrowTilde
	};
	("<-[") => {
		$crate::opengql::token::TokenKind::LeftArrowBracket
	};
	("<~[") => {
		$crate::opengql::token::TokenKind::LeftArrowTildeBracket
	};
	("<->") => {
		$crate::opengql::token::TokenKind::LeftMinusRight
	};
	("<-/") => {
		$crate::opengql::token::TokenKind::LeftMinusSlash
	};
	("<~/") => {
		$crate::opengql::token::TokenKind::LeftTildeSlash
	};
	("<=") => {
		$crate::opengql::token::TokenKind::Lte
	};
	("-[") => {
		$crate::opengql::token::TokenKind::MinusLeftBracket
	};
	("-/") => {
		$crate::opengql::token::TokenKind::MinusSlash
	};
	("<>") => {
		$crate::opengql::token::TokenKind::Neq
	};
	("->") => {
		$crate::opengql::token::TokenKind::RightArrow
	};
	("]-") => {
		$crate::opengql::token::TokenKind::RightBracketMinus
	};
	("]~") => {
		$crate::opengql::token::TokenKind::RightBracketTilde
	};
	("=>") => {
		$crate::opengql::token::TokenKind::RightDoubleArrow
	};
	("/-") => {
		$crate::opengql::token::TokenKind::SlashMinus
	};
	("/->") => {
		$crate::opengql::token::TokenKind::SlashMinusRight
	};
	("/~") => {
		$crate::opengql::token::TokenKind::SlashTilde
	};
	("/~>") => {
		$crate::opengql::token::TokenKind::SlashTildeRight
	};
	("~[") => {
		$crate::opengql::token::TokenKind::TildeLeftBracket
	};
	("~>") => {
		$crate::opengql::token::TokenKind::TildeRightArrow
	};
	("~/") => {
		$crate::opengql::token::TokenKind::TildeSlash
	};

	("&") => {
		$crate::opengql::token::TokenKind::Ampersand
	};
	("@") => {
		$crate::opengql::token::TokenKind::At
	};
	(":") => {
		$crate::opengql::token::TokenKind::Colon
	};
	(",") => {
		$crate::opengql::token::TokenKind::Comma
	};
	("=") => {
		$crate::opengql::token::TokenKind::Eq
	};
	("!") => {
		$crate::opengql::token::TokenKind::Exclamation
	};
	(">") => {
		$crate::opengql::token::TokenKind::Gt
	};
	("<") => {
		$crate::opengql::token::TokenKind::Lt
	};
	("-") => {
		$crate::opengql::token::TokenKind::Minus
	};
	("%") => {
		$crate::opengql::token::TokenKind::Percent
	};
	(".") => {
		$crate::opengql::token::TokenKind::Period
	};
	("+") => {
		$crate::opengql::token::TokenKind::Plus
	};
	("?") => {
		$crate::opengql::token::TokenKind::Question
	};
	("/") => {
		$crate::opengql::token::TokenKind::Slash
	};
	("*") => {
		$crate::opengql::token::TokenKind::Star
	};
	("~") => {
		$crate::opengql::token::TokenKind::Tilde
	};
	("|") => {
		$crate::opengql::token::TokenKind::VerticalBar
	};

	($t:tt) => {
		$crate::opengql::token::TokenKind::Keyword($crate::opengql::token::keyword_t!($t))
	};
}

pub(crate) use t;
