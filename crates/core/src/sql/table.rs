use crate::sql::{Id, Ident, Thing, escape::EscapeIdent, fmt::Fmt, strand::no_nul_bytes};
use revision::revisioned;
use serde::{Deserialize, Serialize};
use std::fmt::{self, Display, Formatter};
use std::ops::Deref;
use std::str;

pub(crate) const TOKEN: &str = "$surrealdb::private::sql::Table";

#[revisioned(revision = 1)]
#[derive(Clone, Debug, Default, Eq, PartialEq, PartialOrd, Serialize, Deserialize, Hash, Ord)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[non_exhaustive]
pub struct Tables(pub Vec<Table>);

impl From<Table> for Tables {
	fn from(v: Table) -> Self {
		Tables(vec![v])
	}
}

impl Deref for Tables {
	type Target = Vec<Table>;
	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

impl Display for Tables {
	fn fmt(&self, f: &mut Formatter) -> fmt::Result {
		Display::fmt(&Fmt::comma_separated(&self.0), f)
	}
}

impl From<Tables> for crate::expr::Tables {
	fn from(v: Tables) -> Self {
		Self(v.0.into_iter().map(Into::into).collect())
	}
}

impl From<crate::expr::Tables> for Tables {
	fn from(v: crate::expr::Tables) -> Self {
		Self(v.0.into_iter().map(Into::into).collect())
	}
}

#[revisioned(revision = 1)]
#[derive(Clone, Debug, Default, Eq, PartialEq, PartialOrd, Serialize, Deserialize, Hash, Ord)]
#[serde(rename = "$surrealdb::private::sql::Table")]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[non_exhaustive]
pub struct Table(#[serde(with = "no_nul_bytes")] pub String);

impl From<String> for Table {
	fn from(v: String) -> Self {
		Self(v)
	}
}

impl From<&str> for Table {
	fn from(v: &str) -> Self {
		Self::from(String::from(v))
	}
}

impl From<Ident> for Table {
	fn from(v: Ident) -> Self {
		Self(v.0)
	}
}

impl From<Table> for crate::expr::Table {
	fn from(v: Table) -> Self {
		crate::expr::Table(v.0)
	}
}

impl From<crate::expr::Table> for Table {
	fn from(v: crate::expr::Table) -> Self {
		Self(v.0)
	}
}

impl Deref for Table {
	type Target = String;
	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

impl Table {
	pub fn generate(&self) -> Thing {
		Thing {
			tb: self.0.clone(),
			id: Id::rand(),
		}
	}
}

impl Display for Table {
	fn fmt(&self, f: &mut Formatter) -> fmt::Result {
		EscapeIdent(&self.0).fmt(f)
	}
}
