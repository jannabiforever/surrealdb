//! Graph edge scanning operator for the streaming execution engine.
//!
//! This operator scans graph edges based on a source record, direction, and
//! target edge tables. It is used to implement graph traversal idioms like
//! `person:alice->knows->person`.

use std::sync::Arc;

use futures::StreamExt;

use super::common::{extract_record_ids_into, resolve_record_batch, resolve_version_stamp};
// Re-exported so the existing `pub use graph::{EdgeTableSpec, ...}` surface is
// unchanged after the key-scan machinery moved into `graph_keys`.
pub use super::graph_keys::EdgeTableSpec;
use super::graph_keys::{compute_graph_ranges, decode_graph_edge};
use crate::exec::parts::LookupDirection;
use crate::exec::permission::{PhysicalPermission, should_check_perms};
use crate::exec::{
	AccessMode, ContextLevel, ControlFlowExt, ExecOperator, ExecutionContext, FlowResult,
	OperatorMetrics, PhysicalExpr, ValueBatch, ValueBatchStream, buffer_stream, monitor_stream,
};
use crate::expr::{ControlFlow, Dir};
use crate::iam::Action;
use crate::idx::planner::ScanDirection;
use crate::kvs::CachePolicy;
use crate::val::{RecordId, TableName};

/// What kind of output the GraphEdgeScan should produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GraphScanOutput {
	/// Return the target record IDs (e.g., `person:bob`)
	#[default]
	TargetId,
	/// Return the full edge records (fetched from the datastore)
	FullEdge,
	/// Skip the edge entirely and return the final target vertex.
	///
	/// Used when a `->edge->vertex` traversal touches no edge data and the
	/// edge table has no permissions / events that need to gate access.
	/// New-format adjacency keys carry the target vertex directly; legacy
	/// keys trigger an inline fallback scan of the edge's own adjacency to
	/// recover the target, preserving correctness on unmigrated data.
	TargetVertex,
}

/// Scans graph edges for records received from a child operator stream.
///
/// This operator implements a nested-loop-join pattern: it reads RecordIds from
/// its `input` child operator stream, then for each RecordId scans graph edges
/// in the specified direction and target tables. It produces a stream of either
/// target IDs or full edge records depending on the output mode.
///
/// This forms part of a streaming DAG where the data flows explicitly:
/// ```text
/// CurrentValueSource → GraphEdgeScan("knows") → GraphEdgeScan("person")
/// ```
#[derive(Debug, Clone)]
pub struct GraphEdgeScan {
	/// Child operator that provides source RecordId(s)
	pub(crate) input: Arc<dyn ExecOperator>,

	/// Direction of the edge traversal (In = `<-`, Out = `->`, Both = `<->`)
	pub(crate) direction: LookupDirection,

	/// Target edge table(s) to scan, optionally with range bounds.
	/// If empty, scans all edge tables in that direction.
	pub(crate) edge_tables: Vec<EdgeTableSpec>,

	/// What to output: EdgeId, TargetId, or FullEdge
	pub(crate) output_mode: GraphScanOutput,

	/// Filter on the final vertex table(s) for `TargetVertex` mode.
	///
	/// Populated by the planner when collapsing `->edge->vertex` into a
	/// single scan. New-format keys whose embedded target table does not
	/// match this list are skipped; legacy keys trigger an inline fallback
	/// that scans the edge's adjacency restricted to these tables. The
	/// planner only produces this scan when the next-hop vertex tables are
	/// known (see `Planner::try_fast_path_pair`), so this is always
	/// non-empty in practice; an empty list is treated defensively as
	/// "match any target", but no current call site constructs that case.
	pub(crate) target_tables: Vec<TableName>,

	/// Optional VERSION timestamp for time-travel queries.
	pub(crate) version: Option<Arc<dyn PhysicalExpr>>,

	/// Optional limit on the total number of edges yielded per source record.
	/// When set, edge scanning stops early after this many results.
	pub(crate) limit: Option<usize>,

	/// Per-operator runtime metrics for EXPLAIN ANALYZE.
	pub(crate) metrics: Arc<OperatorMetrics>,
}

impl GraphEdgeScan {
	pub(crate) fn new(
		input: Arc<dyn ExecOperator>,
		direction: LookupDirection,
		edge_tables: Vec<EdgeTableSpec>,
		output_mode: GraphScanOutput,
		version: Option<Arc<dyn PhysicalExpr>>,
	) -> Self {
		Self {
			input,
			direction,
			edge_tables,
			output_mode,
			target_tables: Vec::new(),
			version,
			limit: None,
			metrics: Arc::new(OperatorMetrics::new()),
		}
	}

	pub(crate) fn with_limit(mut self, limit: usize) -> Self {
		self.limit = Some(limit);
		self
	}

	/// Configure the next-hop target vertex tables. Only meaningful when
	/// `output_mode == TargetVertex`; ignored otherwise.
	pub(crate) fn with_target_tables(mut self, tables: Vec<TableName>) -> Self {
		self.target_tables = tables;
		self
	}
}
impl ExecOperator for GraphEdgeScan {
	fn name(&self) -> &'static str {
		"GraphEdgeScan"
	}

	fn attrs(&self) -> Vec<(String, String)> {
		let dir = match self.direction {
			LookupDirection::Out => "->",
			LookupDirection::In => "<-",
			LookupDirection::Both => "<->",
			LookupDirection::Reference => "<~",
		};
		let tables = if self.edge_tables.is_empty() {
			"*".to_string()
		} else {
			self.edge_tables.iter().map(|t| t.table.as_str()).collect::<Vec<_>>().join(", ")
		};
		let mut attrs = vec![
			("direction".to_string(), dir.to_string()),
			("tables".to_string(), tables),
			("output".to_string(), format!("{:?}", self.output_mode)),
		];
		if let Some(ref version) = self.version {
			attrs.push(("version".to_string(), version.to_sql()));
		}
		if let Some(limit) = self.limit {
			attrs.push(("limit".to_string(), limit.to_string()));
		}
		attrs
	}

	fn required_context(&self) -> ContextLevel {
		// GraphEdgeScan needs database context, combined with expression contexts
		self.input.required_context().max(ContextLevel::Database)
	}

	fn access_mode(&self) -> AccessMode {
		let mut mode = self.input.access_mode();
		if let Some(ref version) = self.version {
			mode = mode.combine(version.access_mode());
		}
		mode
	}

	fn metrics(&self) -> Option<&OperatorMetrics> {
		Some(&self.metrics)
	}

	fn children(&self) -> Vec<&Arc<dyn ExecOperator>> {
		vec![&self.input]
	}

	fn execute(&self, ctx: &ExecutionContext) -> FlowResult<ValueBatchStream> {
		let db_ctx = ctx.database()?.clone();
		// SECURITY: graph edge results bypass `Document::pluck_select`, so we
		// must enforce the edge/target table's SELECT permission here. Without
		// this check a low-privileged user could traverse `->edge` to
		// enumerate otherwise-hidden relationships, and the `FullEdge` output
		// mode would additionally return raw record data for tables they
		// cannot SELECT. `resolve_record_batch` enforces the table-level
		// permission and — in `FullEdge` mode — additionally applies
		// field-level SELECT permissions and computed fields, matching a
		// direct `SELECT *` on the edge table.
		let check_perms = should_check_perms(&db_ctx, Action::View)?;
		let input_stream = buffer_stream(
			self.input.execute(ctx)?,
			self.input.access_mode(),
			self.input.cardinality_hint(),
			ctx.root().ctx.config.operator_buffer_size,
		);
		let direction = self.direction;
		let edge_tables = self.edge_tables.clone();
		let output_mode = self.output_mode;
		let target_tables = self.target_tables.clone();
		let edge_limit = self.limit;
		let version_expr = self.version.clone();
		let scan_batch_size = ctx.root().ctx.config.scan_batch_size;
		let ctx = ctx.clone();
		let fetch_full = output_mode == GraphScanOutput::FullEdge;

		let stream = async_stream::try_stream! {
			let txn = ctx.txn();
			let ns_id = db_ctx.ns_ctx.ns.namespace_id;
			let db_id = db_ctx.db.database_id;
			// Compiled SELECT permissions, cached by table. Different edge
			// tables may have different permission policies, so we resolve
			// lazily per table on first use.
			let mut perm_cache: std::collections::HashMap<
				crate::val::TableName,
				PhysicalPermission,
			> = std::collections::HashMap::new();

			let version: Option<u64> = resolve_version_stamp(&ctx, version_expr.as_ref()).await?;

			// Determine the directions to scan
			// Note: For Both, we scan In first then Out to match legacy executor behavior
			let directions: Vec<Dir> = match direction {
				LookupDirection::Out => vec![Dir::Out],
				LookupDirection::In => vec![Dir::In],
				LookupDirection::Both => vec![Dir::In, Dir::Out],
				LookupDirection::Reference => {
					Err(ControlFlow::Err(anyhow::anyhow!(
						"Reference lookups should use ReferenceScan, not GraphEdgeScan"
					)))?
				}
			};

			// Read from the child operator stream and extract RecordIds
			futures::pin_mut!(input_stream);
			let mut rid_batch: Vec<RecordId> = Vec::with_capacity(scan_batch_size);

			while let Some(batch_result) = input_stream.next().await {
				let batch = batch_result?;
				let source_rids: Vec<RecordId> = batch.values
					.into_iter()
					.flat_map(|v| {
						let mut rids = Vec::new();
						extract_record_ids_into(v, &mut rids);
						rids
					})
					.collect();

				// Scan edges for each source record
				for rid in &source_rids {
					let mut edges_yielded: usize = 0;
					'dir_loop: for &dir in &directions {
						let ranges = compute_graph_ranges(
							ns_id, db_id, rid, dir, &edge_tables, &ctx,
						).await?;

						for (beg, end) in ranges {
							// Outer cursor over the source vertex's adjacency.
							// In `TargetVertex` mode, legacy-format keys (no
							// embedded target) are buffered into a bounded
							// chunk and resolved after the cursor closes,
							// since opening a second cursor on `txn` while
							// the outer cursor still borrows it is not
							// allowed. When the buffer fills before the
							// range is exhausted, we drain it via inner
							// scans and resume the outer cursor past the
							// last processed key.
							let mut current_beg = beg;
							let mut limit_hit = false;
							'range_chunks: loop {
								let mut legacy_edges: Vec<RecordId> = Vec::new();
								let mut chunk_bound_hit = false;
								let mut last_processed_key: Option<Vec<u8>> = None;
								{
									let mut cursor = txn
										.open_keys_cursor(
											current_beg.clone()..end.clone(),
											ScanDirection::Forward,
											0,
											version,
										)
										.await
										.context("Failed to open graph cursor")?;
									'cursor_loop: loop {
										// Cap each batch to the remaining edge
										// budget (when `edge_limit` is set) so we
										// don't over-fetch past the user's LIMIT.
										let remaining = edge_limit.map(|l| {
											l.saturating_sub(edges_yielded)
												.min(crate::kvs::NORMAL_BATCH_SIZE as usize)
										});
										let batch_size = remaining
											.map(|r| r as u32)
											.unwrap_or(crate::kvs::NORMAL_BATCH_SIZE);
										if batch_size == 0 {
											limit_hit = true;
											break;
										}
										let batch = cursor
											.next_batch(batch_size)
											.await
											.context("Failed to scan graph edge")?;
										if batch.is_empty() {
											break;
										}
										for key in &batch {
											let decoded = decode_graph_edge(key)?;
											if output_mode == GraphScanOutput::TargetVertex {
												match decoded.target {
													// New-format key: the embedded
													// target vertex lets us skip
													// the edge-record hop entirely.
													Some(target)
														if target_tables.is_empty()
															|| target_tables
																.contains(&target.table) =>
													{
														rid_batch.push(target);
														edges_yielded += 1;
													}
													Some(_) => {
														// Target table doesn't
														// match the next-hop
														// filter; skip.
													}
													None => {
														// Legacy key: defer the
														// fallback scan until the
														// outer cursor closes
														// (see below). Bound the
														// buffer to `scan_batch_size`
														// so a vertex with many
														// un-migrated edges
														// doesn't OOM the scan.
														legacy_edges.push(decoded.edge);
														if legacy_edges.len()
															>= scan_batch_size
														{
															chunk_bound_hit = true;
															last_processed_key =
																Some(key.to_vec());
															break;
														}
													}
												}
											} else {
												// `TargetId` / `FullEdge` operate
												// on the edge identity (unchanged
												// behavior).
												rid_batch.push(decoded.edge);
												edges_yielded += 1;
											}
											if edge_limit.is_some_and(|l| edges_yielded >= l) {
												limit_hit = true;
												break;
											}
										}
										// `batch`'s borrow of the cursor ends with
										// the for-loop above (NLL), so it's safe
										// to await `resolve_record_batch` here.
										// Flushing inside the cursor loop bounds
										// `rid_batch` to ~`scan_batch_size +
										// NORMAL_BATCH_SIZE` even for high-fanout
										// edge ranges.
										if rid_batch.len() >= scan_batch_size {
											let values = resolve_record_batch(
												&ctx,
												&txn,
												ns_id,
												db_id,
												&rid_batch,
												fetch_full,
												check_perms,
												version,
												CachePolicy::ReadWrite,
												&mut perm_cache,
											)
											.await?;
											yield ValueBatch {
												values,
											};
											rid_batch.clear();
										}
										if limit_hit || chunk_bound_hit {
											break 'cursor_loop;
										}
									}
									drop(cursor);
								}

								// Legacy-format fallback: for each adjacency entry
								// that did not embed a target, walk the edge's
								// own adjacency (restricted to the requested
								// target tables) to recover the target vertex.
								// Runs after the outer cursor is dropped so we
								// can hold an inner cursor on `txn`.
								if !legacy_edges.is_empty() && !limit_hit {
									let inner_specs: Vec<EdgeTableSpec> =
										if target_tables.is_empty() {
											Vec::new()
										} else {
											target_tables
												.iter()
												.cloned()
												.map(|t| EdgeTableSpec {
													table: t,
													range_start: std::ops::Bound::Unbounded,
													range_end: std::ops::Bound::Unbounded,
												})
												.collect()
										};
									'legacy_loop: for edge_rid in legacy_edges {
										let inner_ranges = compute_graph_ranges(
											ns_id,
											db_id,
											&edge_rid,
											dir,
											&inner_specs,
											&ctx,
										)
										.await?;
										for (ibeg, iend) in inner_ranges {
											let mut inner_cursor = txn
												.open_keys_cursor(
													ibeg..iend,
													ScanDirection::Forward,
													0,
													version,
												)
												.await
												.context(
													"Failed to open legacy-fallback graph cursor",
												)?;
											loop {
												let inner_batch = inner_cursor
													.next_batch(crate::kvs::NORMAL_BATCH_SIZE)
													.await
													.context(
														"Failed to scan edge adjacency for legacy graph fallback",
													)?;
												if inner_batch.is_empty() {
													break;
												}
												for ik in &inner_batch {
													// On edge-side adjacency keys the
													// `(ft, fk)` slot holds the endpoint
													// vertex, not an edge -- the legacy
													// fallback walks the edge's own
													// adjacency precisely to recover
													// that vertex. Bind it under a name
													// that reflects what it actually is.
													let endpoint = decode_graph_edge(ik)?.edge;
													rid_batch.push(endpoint);
													edges_yielded += 1;
													if edge_limit
														.is_some_and(|l| edges_yielded >= l)
													{
														limit_hit = true;
														break;
													}
												}
												if rid_batch.len() >= scan_batch_size {
													let values = resolve_record_batch(
														&ctx,
														&txn,
														ns_id,
														db_id,
														&rid_batch,
														fetch_full,
														check_perms,
														version,
														CachePolicy::ReadWrite,
														&mut perm_cache,
													)
													.await?;
													yield ValueBatch {
														values,
													};
													rid_batch.clear();
												}
												if limit_hit {
													break;
												}
											}
											drop(inner_cursor);
											if limit_hit {
												break 'legacy_loop;
											}
										}
									}
								}

								// Continue chunking only when the cursor was
								// suspended because the legacy buffer filled.
								// `last_processed_key` is the legacy key that
								// triggered the bound; resume past it with
								// `0xff` (the same sentinel used by the range
								// bounds, see `eval_graph_bound`).
								if !chunk_bound_hit || limit_hit {
									break 'range_chunks;
								}
								let mut next_beg = last_processed_key
									.expect("chunk_bound_hit implies a key was processed");
								next_beg.push(0xff);
								current_beg = next_beg;
							}

							if limit_hit {
								break 'dir_loop;
							}
						}
					}
				}
			}

			// Yield remaining batch
			if !rid_batch.is_empty() {
				let values = resolve_record_batch(
					&ctx, &txn, ns_id, db_id, &rid_batch, fetch_full, check_perms, version,
					CachePolicy::ReadWrite, &mut perm_cache,
				).await?;
				yield ValueBatch { values };
			}
		};

		Ok(monitor_stream(Box::pin(stream), "GraphEdgeScan", &self.metrics))
	}
}

#[cfg(test)]
mod tests {
	use std::ops::Bound;

	use super::*;
	use crate::exec::operators::CurrentValueSource;

	#[test]
	fn test_graph_edge_scan_attrs() {
		let scan = GraphEdgeScan::new(
			Arc::new(CurrentValueSource::new()),
			LookupDirection::Out,
			vec![
				EdgeTableSpec {
					table: "knows".into(),
					range_start: Bound::Unbounded,
					range_end: Bound::Unbounded,
				},
				EdgeTableSpec {
					table: "follows".into(),
					range_start: Bound::Unbounded,
					range_end: Bound::Unbounded,
				},
			],
			GraphScanOutput::TargetId,
			None,
		);

		assert_eq!(scan.name(), "GraphEdgeScan");
		let attrs = scan.attrs();
		assert!(attrs.iter().any(|(k, v)| k == "direction" && v == "->"));
		assert!(attrs.iter().any(|(k, v)| k == "tables" && v.contains("knows")));
	}
}
