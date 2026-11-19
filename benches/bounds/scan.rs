//! What a query reads, from DataFusion's optimized plan, and what DataFusion
//! does for each scan run on its own, for cross-checking

use std::collections::BTreeSet;
use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;

use datafusion::arrow::array::RecordBatch;
use datafusion::arrow::datatypes::SchemaRef;
use datafusion::common::tree_node::{Transformed, TreeNode, TreeNodeRecursion};
use datafusion::common::{Column, TableReference};
use datafusion::datasource::listing::ListingTable;
use datafusion::datasource::source_as_provider;
use datafusion::logical_expr::utils::conjunction;
use datafusion::logical_expr::{Expr, LogicalPlan};
use datafusion::physical_plan::aggregates::{AggregateExec, AggregateMode};
use datafusion::physical_plan::metrics::{MetricValue, MetricsSet};
use datafusion::physical_plan::sorts::sort::SortExec;
use datafusion::physical_plan::{ExecutionPlan, collect, execute_stream};
use datafusion::prelude::SessionContext;
use futures::StreamExt;

/// One table scan in a query's plan
pub(crate) struct Scan {
    pub(crate) table: String,
    pub(crate) path: PathBuf,
    /// The table's full schema, as DataFusion reads it
    pub(crate) schema: SchemaRef,
    /// Columns read: output and filter columns, in table order
    pub(crate) columns: Vec<String>,
    /// Filters pushed into the scan, all of which must hold, with column names unqualified
    pub(crate) filters: Vec<Expr>,
}

impl Scan {
    pub(crate) fn filter_columns(&self) -> BTreeSet<String> {
        self.filters
            .iter()
            .flat_map(|filter| filter.column_refs().into_iter().map(|c| c.name.clone()))
            .collect()
    }
}

/// What DataFusion pruned and read for one scan, run on its own
#[derive(Default, Clone, Copy)]
pub(crate) struct DfScan {
    pub(crate) row_groups_pruned: usize,
    pub(crate) pages_pruned: usize,
    pub(crate) bytes_scanned: usize,
}

impl DfScan {
    pub(crate) fn add(&mut self, other: Self) {
        self.row_groups_pruned += other.row_groups_pruned;
        self.pages_pruned += other.pages_pruned;
        self.bytes_scanned += other.bytes_scanned;
    }
}

/// A query statement's scans and what running it produced
pub(crate) struct Run {
    pub(crate) scans: Vec<Scan>,
    pub(crate) output_rows: usize,
    /// Rows out of each final aggregate, outermost first. `None` where a
    /// `LIMIT` let DataFusion stop aggregating early: how far it got depends
    /// on thread timing, so it isn't the number of groups
    pub(crate) groups: Vec<Option<usize>>,
}

/// Whether `statement` returns rows, as opposed to DDL such as TPC-H Q15's view
pub(crate) fn is_query(statement: &str) -> bool {
    let start = statement.trim_start().to_ascii_lowercase();
    start.starts_with("select") || start.starts_with("with") || start.starts_with('(')
}

/// Plans and runs one statement
pub(crate) async fn run(ctx: &SessionContext, sql: &str) -> Result<Run, Box<dyn Error>> {
    let frame = ctx.sql(sql).await?;
    let scans = scans(&frame.clone().into_optimized_plan()?)?;
    let plan = frame.create_physical_plan().await?;
    let batches = collect(Arc::clone(&plan), ctx.task_ctx()).await?;
    let mut groups = Vec::new();
    final_aggregates(&plan, false, &mut groups);
    Ok(Run {
        scans,
        output_rows: batches.iter().map(RecordBatch::num_rows).sum(),
        groups,
    })
}

/// Runs `scan` on its own in DataFusion: its columns, with its filters pushed
/// into the scan. Inside the whole query, DataFusion can also prune with
/// filters it builds while running (from a join's other side, or a top-k),
/// and several scans of one file can't be told apart in its metrics. On its
/// own, the scan has exactly the static problem the bounds solve
pub(crate) async fn datafusion_scan(
    ctx: &SessionContext,
    scan: &Scan,
) -> Result<DfScan, Box<dyn Error>> {
    if scan.columns.is_empty() {
        // Reads no column (`COUNT(*)`): nothing to prune or read
        return Ok(DfScan::default());
    }
    let columns = scan
        .columns
        .iter()
        .map(|name| Expr::Column(Column::new_unqualified(name)))
        .collect::<Vec<_>>();
    let mut frame = ctx
        .table(TableReference::bare(scan.table.as_str()))
        .await?
        .select(columns)?;
    if let Some(filter) = conjunction(scan.filters.iter().cloned()) {
        frame = frame.filter(filter)?;
    }
    let plan = frame.create_physical_plan().await?;
    // Drained batch by batch: the scan's rows can be far larger than memory
    let mut stream = execute_stream(Arc::clone(&plan), ctx.task_ctx())?;
    while let Some(batch) = stream.next().await {
        batch?;
    }

    let mut sources = Vec::new();
    data_sources(&plan, &mut sources);
    let [metrics] = sources.as_slice() else {
        return Err(format!(
            "running scan of {} alone: expected one DataSourceExec, found {}",
            scan.table,
            sources.len()
        )
        .into());
    };
    let mut out = DfScan::default();
    for metric in metrics.iter() {
        match metric.value() {
            MetricValue::PruningMetrics {
                name,
                pruning_metrics,
            } if name == "row_groups_pruned_statistics" => {
                out.row_groups_pruned += pruning_metrics.pruned();
            }
            MetricValue::PruningMetrics {
                name,
                pruning_metrics,
            } if name == "page_index_pages_pruned" => {
                out.pages_pruned += pruning_metrics.pruned();
            }
            MetricValue::Count { name, count } if name == "bytes_scanned" => {
                out.bytes_scanned += count.value();
            }
            _ => {}
        }
    }
    Ok(out)
}

fn scans(plan: &LogicalPlan) -> Result<Vec<Scan>, Box<dyn Error>> {
    let mut found = Vec::new();
    let mut failed = None;
    plan.apply_with_subqueries(|node| {
        if let LogicalPlan::TableScan(scan) = node {
            match to_scan(scan) {
                Ok(Some(scan)) => found.push(scan),
                Ok(None) => {}
                Err(err) => failed = Some(err),
            }
        }
        Ok(TreeNodeRecursion::Continue)
    })?;
    match failed {
        Some(err) => Err(err),
        None => Ok(found),
    }
}

/// `None` for scans of anything but a Parquet listing table
fn to_scan(scan: &datafusion::logical_expr::TableScan) -> Result<Option<Scan>, Box<dyn Error>> {
    let provider = source_as_provider(&scan.source)?;
    let Some(listing) = provider.downcast_ref::<ListingTable>() else {
        return Ok(None);
    };
    let Some(url) = listing.table_paths().first() else {
        return Ok(None);
    };
    let path = PathBuf::from(
        url.as_str()
            .strip_prefix("file://")
            .ok_or_else(|| format!("not a local file: {}", url.as_str()))?,
    );
    let schema = provider.schema();
    let filters = scan
        .filters
        .iter()
        .map(|filter| unqualify(filter.clone()))
        .collect::<Result<Vec<_>, _>>()?;

    let mut wanted: BTreeSet<usize> = match &scan.projection {
        Some(indices) => indices.iter().copied().collect(),
        None => (0..schema.fields().len()).collect(),
    };
    for filter in &filters {
        for column in filter.column_refs() {
            wanted.extend(schema.index_of(&column.name).ok());
        }
    }
    Ok(Some(Scan {
        table: scan.table_name.table().to_string(),
        path,
        columns: wanted
            .into_iter()
            .map(|i| schema.field(i).name().clone())
            .collect(),
        schema,
        filters,
    }))
}

fn unqualify(expr: Expr) -> datafusion::error::Result<Expr> {
    expr.transform(|e| match e {
        Expr::Column(column) => Ok(Transformed::yes(Expr::Column(Column::new_unqualified(
            column.name,
        )))),
        other => Ok(Transformed::no(other)),
    })
    .map(|t| t.data)
}

/// Rows out of each final aggregate in `plan`, outermost first (see
/// [`Run::groups`]). `cut` is whether an operator above may stop pulling
/// rows early (it has a `fetch`, as `LIMIT` gives), with nothing in between
/// that reads all of its input first
fn final_aggregates(plan: &Arc<dyn ExecutionPlan>, cut: bool, out: &mut Vec<Option<usize>>) {
    // A sort, top-k included, reads all of its input before emitting
    let sorts = plan.downcast_ref::<SortExec>().is_some();
    let mut cut = !sorts && (cut || plan.fetch().is_some());
    if let Some(aggregate) = plan.downcast_ref::<AggregateExec>() {
        // An aggregate with a limit may also stop reading its input early
        let limited = aggregate.limit_options().is_some();
        if matches!(
            aggregate.mode(),
            AggregateMode::Final
                | AggregateMode::FinalPartitioned
                | AggregateMode::Single
                | AggregateMode::SinglePartitioned
        ) {
            out.push((!cut && !limited).then(|| {
                plan.metrics()
                    .as_ref()
                    .and_then(MetricsSet::output_rows)
                    .unwrap_or(0)
            }));
        }
        cut = limited;
    }
    for child in plan.children() {
        final_aggregates(child, cut, out);
    }
}

/// The metrics of every `DataSourceExec` in `plan`
fn data_sources(plan: &Arc<dyn ExecutionPlan>, out: &mut Vec<MetricsSet>) {
    if plan.name() == "DataSourceExec" {
        out.push(plan.metrics().unwrap_or_default());
    }
    for child in plan.children() {
        data_sources(child, out);
    }
}
