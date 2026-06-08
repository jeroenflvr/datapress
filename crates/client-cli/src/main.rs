//! `datapress-cli` — a command-line client for a DataPress dataset server.
//!
//! Thin wrapper over [`datapress_client::blocking::Client`]. Connection
//! settings come from flags or environment variables
//! (`DATAPRESS_URL`, `DATAPRESS_TOKEN`, `DATAPRESS_ADMIN_TOKEN`).

use std::fs;
use std::io::Write;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use datapress_client::blocking::Client;
use datapress_client::{Aggregation, OrderBy, Predicate, QueryRequest};
use serde_json::Value as JsonValue;

/// Command-line client for a DataPress dataset server.
#[derive(Debug, Parser)]
#[command(name = "datapress-cli", version, about, long_about = None)]
struct Cli {
    #[command(flatten)]
    conn: ConnectionArgs,

    #[command(subcommand)]
    command: Command,
}

/// Shared connection options.
#[derive(Debug, Args)]
struct ConnectionArgs {
    /// Server base URL (include any configured server prefix).
    #[arg(
        long,
        global = true,
        env = "DATAPRESS_URL",
        default_value = "http://127.0.0.1:8000"
    )]
    url: String,

    /// Versioned API mount path.
    #[arg(long, global = true, default_value = "/api/v1")]
    api_base: String,

    /// OAuth2 bearer token (servers with auth enabled).
    #[arg(long, global = true, env = "DATAPRESS_TOKEN")]
    bearer_token: Option<String>,

    /// Admin token sent as `X-Admin-Token` (required by `reload`).
    #[arg(long, global = true, env = "DATAPRESS_ADMIN_TOKEN")]
    admin_token: Option<String>,

    /// Per-request timeout, in seconds.
    #[arg(long, global = true)]
    timeout: Option<f64>,

    /// Pretty-print JSON output (default is compact, single-line).
    #[arg(long, global = true)]
    pretty: bool,
}

impl ConnectionArgs {
    fn build(&self) -> Result<Client> {
        let mut builder = Client::builder(&self.url).api_base(&self.api_base);
        if let Some(t) = &self.bearer_token {
            builder = builder.bearer_token(t);
        }
        if let Some(t) = &self.admin_token {
            builder = builder.admin_token(t);
        }
        if let Some(secs) = self.timeout {
            builder = builder.timeout(std::time::Duration::from_secs_f64(secs));
        }
        let async_client = builder.build().context("failed to build client")?;
        Client::from_async(async_client).context("failed to start runtime")
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List registered dataset names.
    Datasets,

    /// Print the schema of a dataset.
    Schema {
        /// Dataset name.
        dataset: String,
    },

    /// Count matching rows.
    Count {
        /// Dataset name.
        dataset: String,
        /// Filter, repeatable. Format: `col:op:val` (e.g. `Severity:gte:3`)
        /// or `col:op` for `is_null` / `is_not_null`.
        #[arg(long = "where", value_name = "col:op[:val]")]
        filters: Vec<String>,
    },

    /// Run a structured query.
    Query(QueryArgs),

    /// Run a read-only SQL statement (`POST /sql`).
    Sql {
        /// The SQL statement.
        statement: String,
        /// Cap the number of rows returned.
        #[arg(long)]
        max_rows: Option<u64>,
    },

    /// Trigger an in-place reload of a dataset.
    Reload {
        /// Dataset name.
        dataset: String,
    },

    /// Liveness probe (`GET /healthz`).
    Health,

    /// Readiness probe (`GET /readyz`).
    Ready,
}

/// Arguments for the `query` subcommand.
#[derive(Debug, Args)]
struct QueryArgs {
    /// Dataset name.
    dataset: String,

    /// Projected columns (comma-separated, repeatable). Empty = all.
    #[arg(long = "select", value_name = "col,col,…", value_delimiter = ',')]
    columns: Vec<String>,

    /// Filter, repeatable. Format: `col:op:val` or `col:op`.
    #[arg(long = "where", value_name = "col:op[:val]")]
    filters: Vec<String>,

    /// Group-by columns (comma-separated, repeatable).
    #[arg(long = "group-by", value_name = "col,col,…", value_delimiter = ',')]
    group_by: Vec<String>,

    /// Aggregation, repeatable. Format: `op:col[:alias]` or `count[:alias]`.
    #[arg(long = "agg", value_name = "op:col[:alias]")]
    aggregations: Vec<String>,

    /// Post-aggregation filter, repeatable. Same format as `--where`.
    #[arg(long = "having", value_name = "col:op[:val]")]
    having: Vec<String>,

    /// Return only distinct rows.
    #[arg(long)]
    distinct: bool,

    /// Sort key, repeatable. Format: `col` or `col:asc` / `col:desc`.
    #[arg(long = "order-by", value_name = "col[:dir]")]
    order_by: Vec<String>,

    /// Cap the total number of rows returned.
    #[arg(long)]
    limit: Option<u64>,

    /// 1-based page number.
    #[arg(long)]
    page: Option<u64>,

    /// Page size.
    #[arg(long)]
    page_size: Option<u64>,

    /// Fetch the result as Arrow IPC and write the raw stream to this file.
    #[arg(long, value_name = "PATH")]
    arrow_out: Option<String>,

    /// Fetch via Arrow IPC and render an ASCII table instead of JSON.
    #[arg(long)]
    table: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = cli.conn.build()?;
    let pretty = cli.conn.pretty;

    match cli.command {
        Command::Datasets => {
            let names = client.datasets()?;
            print_json(&JsonValue::from(names), pretty)?;
        }
        Command::Schema { dataset } => {
            print_json(&client.schema(&dataset)?, pretty)?;
        }
        Command::Count { dataset, filters } => {
            let preds = parse_predicates(&filters)?;
            let n = client.count(&dataset, &preds)?;
            println!("{n}");
        }
        Command::Query(args) => run_query(&client, args, pretty)?,
        Command::Sql {
            statement,
            max_rows,
        } => {
            let resp = client.sql(statement, max_rows)?;
            print_json(&serde_json::to_value(resp)?, pretty)?;
        }
        Command::Reload { dataset } => {
            print_json(&client.reload(&dataset)?, pretty)?;
        }
        Command::Health => print_json(&client.healthz()?, pretty)?,
        Command::Ready => print_json(&client.readyz()?, pretty)?,
    }

    Ok(())
}

fn run_query(client: &Client, args: QueryArgs, pretty: bool) -> Result<()> {
    let mut builder = QueryRequest::builder();
    if !args.columns.is_empty() {
        builder = builder.columns(args.columns);
    }
    for p in parse_predicates(&args.filters)? {
        builder = builder.predicate(p);
    }
    if !args.group_by.is_empty() {
        builder = builder.group_by(args.group_by);
    }
    for a in &args.aggregations {
        builder = builder.aggregation(parse_aggregation(a)?);
    }
    for h in parse_predicates(&args.having)? {
        builder = builder.having(h);
    }
    if args.distinct {
        builder = builder.distinct(true);
    }
    for o in &args.order_by {
        builder = builder.order_by(parse_order_by(o));
    }
    if let Some(n) = args.limit {
        builder = builder.limit(n);
    }
    if let Some(n) = args.page {
        builder = builder.page(n);
    }
    if let Some(n) = args.page_size {
        builder = builder.page_size(n);
    }
    let request = builder.build();

    if let Some(path) = args.arrow_out {
        let bytes = client.query_arrow_bytes(&args.dataset, &request)?;
        write_bytes(&path, &bytes)?;
        eprintln!("wrote {} bytes of Arrow IPC to {path}", bytes.len());
    } else if args.table {
        let batches = client.query_arrow(&args.dataset, &request)?;
        let rendered = arrow::util::pretty::pretty_format_batches(&batches)
            .context("failed to format record batches")?;
        println!("{rendered}");
    } else {
        let resp = client.query_json(&args.dataset, &request)?;
        print_json(&serde_json::to_value(resp)?, pretty)?;
    }
    Ok(())
}

// --------------------------------------------------------------- parsing --

/// Parse a list of `col:op:val` (or `col:op`) predicate specs.
fn parse_predicates(specs: &[String]) -> Result<Vec<Predicate>> {
    specs.iter().map(|s| parse_predicate(s)).collect()
}

fn parse_predicate(spec: &str) -> Result<Predicate> {
    let mut parts = spec.splitn(3, ':');
    let col = parts
        .next()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("predicate `{spec}`: missing column"))?;
    let op = parts
        .next()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("predicate `{spec}`: missing operator"))?;
    match parts.next() {
        Some(val) => Ok(Predicate::new(col, op, parse_value(val))),
        None => {
            if matches!(op, "is_null" | "is_not_null") {
                Ok(Predicate::unary(col, op))
            } else {
                bail!("predicate `{spec}`: operator `{op}` requires a value (col:op:val)")
            }
        }
    }
}

/// Parse `op:col[:alias]` or `count[:alias]` into an [`Aggregation`].
fn parse_aggregation(spec: &str) -> Result<Aggregation> {
    let parts: Vec<&str> = spec.split(':').collect();
    match parts.as_slice() {
        ["count"] => Ok(Aggregation::count(None)),
        ["count", alias] => Ok(Aggregation::count(Some(alias))),
        [op, col] => Ok(Aggregation::over(*op, *col, None)),
        [op, col, alias] => Ok(Aggregation::over(*op, *col, Some(alias))),
        _ => bail!("aggregation `{spec}`: expected `op:col[:alias]` or `count[:alias]`"),
    }
}

/// Parse `col` / `col:asc` / `col:desc` into an [`OrderBy`].
fn parse_order_by(spec: &str) -> OrderBy {
    match spec.split_once(':') {
        Some((col, "desc")) => OrderBy::desc(col),
        Some((col, _)) => OrderBy::asc(col),
        None => OrderBy::asc(spec),
    }
}

/// Interpret a CLI value as JSON when possible, else a plain string.
fn parse_value(raw: &str) -> JsonValue {
    serde_json::from_str(raw).unwrap_or_else(|_| JsonValue::String(raw.to_string()))
}

// ----------------------------------------------------------------- output --

fn print_json(value: &JsonValue, pretty: bool) -> Result<()> {
    let s = if pretty {
        serde_json::to_string_pretty(value)?
    } else {
        serde_json::to_string(value)?
    };
    println!("{s}");
    Ok(())
}

fn write_bytes(path: &str, bytes: &[u8]) -> Result<()> {
    if path == "-" {
        std::io::stdout()
            .write_all(bytes)
            .context("failed to write to stdout")
    } else {
        fs::write(path, bytes).with_context(|| format!("failed to write {path}"))
    }
}
