//! Running SQL with the simple query protocol, which returns every value as
//! text and so works for every type without per-type decoding.

use std::time::{Duration, Instant};

use futures_util::{StreamExt as _, pin_mut};
use tokio_postgres::{Client, SimpleQueryMessage, error::ErrorPosition};

use crate::db::{QuerySession, describe, pg_err};
use crate::sql::PageQuery;

/// Most rows kept from one statement in a query tab.
pub const MAX_RESULT_ROWS: usize = 10_000;

/// Rows of text values; `None` is SQL NULL.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResultSet {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

/// One statement of a run.
#[derive(Debug, Clone)]
pub struct StatementOutcome {
    /// Rows when the statement returned a result (SELECT, RETURNING, SHOW...).
    pub result: Option<ResultSet>,
    /// Rows returned or affected, as reported by the server.
    pub row_count: u64,
    /// Rows beyond [`MAX_RESULT_ROWS`] were dropped.
    pub truncated: bool,
    pub elapsed: Duration,
}

/// A failed statement, with the server's details.
#[derive(Debug, Clone, Default)]
pub struct QueryError {
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub code: Option<String>,
    /// 0-based character offset into the SQL that was sent.
    pub position: Option<usize>,
    pub connection_lost: bool,
}

impl QueryError {
    fn from_pg(e: &tokio_postgres::Error) -> Self {
        match e.as_db_error() {
            Some(db) => QueryError {
                message: db.message().to_string(),
                detail: db.detail().map(Into::into),
                hint: db.hint().map(Into::into),
                code: Some(db.code().code().to_string()),
                position: match db.position() {
                    Some(ErrorPosition::Original(p)) => Some((*p as usize).saturating_sub(1)),
                    _ => None,
                },
                connection_lost: false,
            },
            None => QueryError {
                message: describe(e),
                connection_lost: e.is_closed(),
                ..Default::default()
            },
        }
    }
}

/// The outcome of running some SQL in a query tab.
#[derive(Debug, Clone, Default)]
pub struct RunOutput {
    /// Statements that completed, in order.
    pub statements: Vec<StatementOutcome>,
    /// The error that stopped the run, if any.
    pub error: Option<QueryError>,
    /// The session is inside a transaction block after the run.
    pub in_transaction: bool,
    pub elapsed: Duration,
}

impl RunOutput {
    /// The last statement that returned rows: what the result grid shows.
    pub fn last_result(&self) -> Option<&StatementOutcome> {
        self.statements.iter().rev().find(|s| s.result.is_some())
    }
}

async fn run_simple(client: &Client, sql: &str, row_cap: usize) -> RunOutput {
    let started = Instant::now();
    let mut out = RunOutput::default();
    let mut checkpoint = started;
    let mut current: Option<ResultSet> = None;
    let mut truncated = false;

    let stream = match client.simple_query_raw(sql).await {
        Ok(s) => s,
        Err(e) => {
            out.error = Some(QueryError::from_pg(&e));
            out.elapsed = started.elapsed();
            return out;
        }
    };
    pin_mut!(stream);
    while let Some(msg) = stream.next().await {
        match msg {
            Ok(SimpleQueryMessage::RowDescription(cols)) => {
                current = Some(ResultSet {
                    columns: cols.iter().map(|c| c.name().to_string()).collect(),
                    rows: Vec::new(),
                });
                truncated = false;
            }
            Ok(SimpleQueryMessage::Row(row)) => {
                if let Some(rs) = current.as_mut() {
                    if rs.rows.len() < row_cap {
                        rs.rows.push(
                            (0..row.len())
                                .map(|i| row.get(i).map(str::to_string))
                                .collect(),
                        );
                    } else {
                        truncated = true;
                    }
                }
            }
            Ok(SimpleQueryMessage::CommandComplete(n)) => {
                let now = Instant::now();
                out.statements.push(StatementOutcome {
                    result: current.take(),
                    row_count: n,
                    truncated,
                    elapsed: now - checkpoint,
                });
                truncated = false;
                checkpoint = now;
            }
            Ok(_) => {}
            Err(e) => {
                out.error = Some(QueryError::from_pg(&e));
                break;
            }
        }
    }
    out.elapsed = started.elapsed();
    out
}

impl QuerySession {
    /// Run everything in `sql` (one or more statements) on this session.
    pub async fn run(&self, sql: &str) -> RunOutput {
        let mut out = run_simple(&self.client, sql, MAX_RESULT_ROWS).await;
        out.in_transaction = self.in_transaction().await;
        out
    }

    /// Whether a transaction block is open. Inside one, `now()` is frozen at
    /// the transaction's start while `statement_timestamp()` moves on; an
    /// aborted transaction rejects the query with SQLSTATE 25P02.
    async fn in_transaction(&self) -> bool {
        // The simple protocol keeps both timestamps equal for a lone
        // statement outside a block; the extended protocol would not.
        match self
            .client
            .simple_query("SELECT now() <> statement_timestamp()")
            .await
        {
            Ok(msgs) => msgs.iter().any(|m| {
                matches!(m, SimpleQueryMessage::Row(r) if r.get(0) == Some("t"))
            }),
            Err(e) => e
                .as_db_error()
                .is_some_and(|db| db.code().code() == "25P02"),
        }
    }
}

/// Fetch one page of a table or view.
pub async fn fetch_page(client: &Client, query: &PageQuery) -> anyhow::Result<ResultSet> {
    let out = run_simple(client, &query.to_sql(), usize::MAX).await;
    if let Some(err) = out.error {
        let e = anyhow::anyhow!(err.message);
        return Err(if err.connection_lost {
            e.context(crate::db::ConnectionLost)
        } else {
            e
        });
    }
    Ok(out
        .statements
        .into_iter()
        .find_map(|s| s.result)
        .unwrap_or_default())
}

/// `SELECT count(*)` of a table or view.
pub async fn count_rows(client: &Client, schema: &str, name: &str) -> anyhow::Result<u64> {
    let row = client
        .query_one(
            &format!("SELECT count(*) FROM {}", crate::sql::qualified(schema, name)),
            &[],
        )
        .await
        .map_err(pg_err)?;
    Ok(row.get::<_, i64>(0) as u64)
}
