use crate::sql::fmt::Fmt;
use crate::sql::idiom::Idiom;
use revision::revisioned;
use serde::{Deserialize, Serialize};
use std::fmt::{self, Display, Formatter};
use std::ops::Deref;

#[revisioned(revision = 1)]
#[derive(Clone, Debug, Default, Eq, PartialEq, PartialOrd, Serialize, Deserialize, Hash)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[non_exhaustive]
pub struct Splits(pub Vec<Split>);

impl Deref for Splits {
	type Target = Vec<Split>;
	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

impl IntoIterator for Splits {
	type Item = Split;
	type IntoIter = std::vec::IntoIter<Self::Item>;
	fn into_iter(self) -> Self::IntoIter {
		self.0.into_iter()
	}
}

impl fmt::Display for Splits {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "SPLIT ON {}", Fmt::comma_separated(&self.0))
	}
}

impl From<Splits> for crate::expr::Splits {
	fn from(v: Splits) -> Self {
		Self(v.0.into_iter().map(Into::into).collect())
	}
}

impl From<crate::expr::Splits> for Splits {
	fn from(v: crate::expr::Splits) -> Self {
		Self(v.0.into_iter().map(Into::into).collect())
	}
}

#[revisioned(revision = 1)]
#[derive(Clone, Debug, Default, Eq, PartialEq, PartialOrd, Serialize, Deserialize, Hash)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[non_exhaustive]
pub struct Split(pub Idiom);

impl Deref for Split {
	type Target = Idiom;
	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

impl Display for Split {
	fn fmt(&self, f: &mut Formatter) -> fmt::Result {
		Display::fmt(&self.0, f)
	}
}

impl From<Split> for crate::expr::Split {
	fn from(v: Split) -> Self {
		Self(v.0.into())
	}
}

impl From<crate::expr::Split> for Split {
	fn from(v: crate::expr::Split) -> Self {
		Self(v.0.into())
	}
}
