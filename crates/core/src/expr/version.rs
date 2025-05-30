use super::FlowResultExt;
use crate::{ctx::Context, dbs::Options, doc::CursorDoc, err::Error, expr::datetime::Datetime};
use anyhow::Result;
use reblessive::tree::Stk;
use revision::revisioned;
use serde::{Deserialize, Serialize};
use std::fmt;

use super::Value;

#[revisioned(revision = 2)]
#[derive(Clone, Debug, Default, Eq, PartialEq, PartialOrd, Serialize, Deserialize, Hash)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[non_exhaustive]
pub struct Version(
	#[revision(end = 2, convert_fn = "convert_version_datetime")] pub Datetime,
	#[revision(start = 2)] pub Value,
);

impl Version {
	fn convert_version_datetime(
		&mut self,
		_revision: u16,
		old: Datetime,
	) -> Result<(), revision::Error> {
		self.0 = Value::Datetime(old);
		Ok(())
	}

	pub(crate) async fn compute(
		&self,
		stk: &mut Stk,
		ctx: &Context,
		opt: &Options,
		doc: Option<&CursorDoc>,
	) -> Result<u64> {
		match self.0.compute(stk, ctx, opt, doc).await.catch_return()? {
			Value::Datetime(v) => match v.to_u64() {
				Some(ts) => Ok(ts),
				_ => Err(anyhow::Error::new(Error::unreachable(
					"Failed to convert datetime to timestamp",
				))),
			},
			found => Err(anyhow::Error::new(Error::InvalidVersion {
				found,
			})),
		}
	}
}

impl fmt::Display for Version {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "VERSION {}", self.0)
	}
}
