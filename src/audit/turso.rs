//! Minimal Turso client for the audit log, speaking Hrana over HTTP.
//!
//! Turso ships an official Rust SDK (`libsql`), and it is the right choice for
//! an application that actually uses a database. This one runs two statements.
//! Pulling `libsql` in with `default-features = false, features = ["remote",
//! "tls"]` adds 37 crates and, because its remote path is built on an older
//! generation of the HTTP stack than `reqwest` already brings, a second copy of
//! rustls, tower, hyper-rustls and thiserror. The `/v2/pipeline` endpoint is a
//! documented, stable JSON protocol, so the audit log speaks it directly and
//! the dependency tree stays as it was.
//!
//! Protocol reference: <https://docs.turso.tech/sdk/http/reference>

use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::conf::admin::Turso as TursoConfig;

use super::{Actor, Record};

const TABLE: &str = "admin_audit_log";

/// An admin request waits on this write, so a wedged database must not hold a
/// handler open indefinitely.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub struct TursoLog {
    client: reqwest::Client,
    /// Base URL without a trailing slash.
    endpoint: String,
    token: String,
}

impl TursoLog {
    /// `Ok(None)` means Turso is simply not configured, which is the default
    /// and not an error. `Err` means it is configured but unusable.
    pub async fn connect(config: &TursoConfig) -> anyhow::Result<Option<Self>> {
        let (Some(endpoint), Some(token)) = (config.endpoint(), config.token()) else {
            return Ok(None);
        };
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .context("failed to build the Turso HTTP client")?;
        let log = TursoLog {
            client,
            endpoint,
            token: token.to_string(),
        };
        log.migrate()
            .await
            .context("failed to prepare the audit table")?;
        Ok(Some(log))
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    async fn migrate(&self) -> anyhow::Result<()> {
        self.pipeline(vec![
            statement(
                &format!(
                    "CREATE TABLE IF NOT EXISTS {TABLE} (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        ts INTEGER NOT NULL,
                        actor TEXT NOT NULL,
                        remote_addr TEXT,
                        action TEXT NOT NULL,
                        target TEXT,
                        success INTEGER NOT NULL,
                        detail TEXT
                    )"
                ),
                vec![],
            ),
            // The panel only ever reads the newest page, so this index is what
            // keeps that query off a full scan as the table grows.
            statement(
                &format!("CREATE INDEX IF NOT EXISTS {TABLE}_ts ON {TABLE} (ts DESC)"),
                vec![],
            ),
        ])
        .await?;
        Ok(())
    }

    pub async fn append(&self, record: Record) -> anyhow::Result<()> {
        self.pipeline(vec![statement(
            &format!(
                "INSERT INTO {TABLE} (ts, actor, remote_addr, action, target, success, detail)
                 VALUES (?, ?, ?, ?, ?, ?, ?)"
            ),
            vec![
                integer(record.ts as i64),
                text(record.actor.as_str()),
                nullable_text(record.remote_addr.as_deref()),
                text(&record.action),
                nullable_text(record.target.as_deref()),
                integer(i64::from(record.success)),
                nullable_text(record.detail.as_deref()),
            ],
        )])
        .await?;
        Ok(())
    }

    pub async fn list(&self, limit: usize, offset: usize) -> anyhow::Result<Vec<Record>> {
        // `id` breaks ties: two records written in the same millisecond would
        // otherwise be ordered arbitrarily, which makes paging skip or repeat.
        let results = self
            .pipeline(vec![statement(
                &format!(
                    "SELECT ts, actor, remote_addr, action, target, success, detail
                     FROM {TABLE} ORDER BY ts DESC, id DESC LIMIT ? OFFSET ?"
                ),
                vec![integer(limit as i64), integer(offset as i64)],
            )])
            .await?;
        let rows = results
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("the pipeline returned no result for the audit query"))?
            .rows;
        rows.iter().map(Vec::as_slice).map(row_to_record).collect()
    }

    /// Runs the statements on one connection and closes it. Turso keeps an
    /// unclosed connection alive until it times out, so the explicit `close`
    /// avoids leaking one per request.
    async fn pipeline(&self, statements: Vec<Value>) -> anyhow::Result<Vec<ExecuteResult>> {
        let expected = statements.len();
        let mut requests = statements;
        requests.push(json!({ "type": "close" }));

        let response = self
            .client
            .post(format!("{}/v2/pipeline", self.endpoint))
            .bearer_auth(&self.token)
            .json(&json!({ "requests": requests }))
            .send()
            .await
            .context("the Turso request failed")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("Turso returned HTTP {status}: {}", body.trim());
        }

        let body: PipelineResponse = response
            .json()
            .await
            .context("failed to decode the Turso response")?;

        let mut results = Vec::with_capacity(expected);
        for step in body.results {
            match step {
                Step::Ok {
                    response: StepResponse::Execute { result },
                } => results.push(result),
                Step::Ok {
                    response: StepResponse::Close,
                } => {}
                Step::Error { error } => bail!("Turso rejected a statement: {}", error.message),
            }
        }
        Ok(results)
    }
}

fn statement(sql: &str, args: Vec<Value>) -> Value {
    json!({ "type": "execute", "stmt": { "sql": sql, "args": args } })
}

fn text(value: &str) -> Value {
    json!({ "type": "text", "value": value })
}

/// Integers travel as strings: JSON numbers are doubles in many decoders, which
/// would silently round a millisecond timestamp.
fn integer(value: i64) -> Value {
    json!({ "type": "integer", "value": value.to_string() })
}

fn nullable_text(value: Option<&str>) -> Value {
    match value {
        Some(value) => text(value),
        None => json!({ "type": "null" }),
    }
}

fn row_to_record(row: &[HranaValue]) -> anyhow::Result<Record> {
    let column = |index: usize| -> anyhow::Result<&HranaValue> {
        row.get(index)
            .ok_or_else(|| anyhow!("the audit row is missing column {index}"))
    };
    Ok(Record {
        ts: column(0)?.as_i64().unwrap_or_default().max(0) as u128,
        actor: Actor::parse(column(1)?.as_str().unwrap_or_default()),
        remote_addr: column(2)?.as_str().map(str::to_string),
        action: column(3)?.as_str().unwrap_or_default().to_string(),
        target: column(4)?.as_str().map(str::to_string),
        success: column(5)?.as_i64().unwrap_or_default() != 0,
        detail: column(6)?.as_str().map(str::to_string),
    })
}

#[derive(Deserialize)]
struct PipelineResponse {
    results: Vec<Step>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Step {
    Ok { response: StepResponse },
    Error { error: HranaError },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum StepResponse {
    Execute { result: ExecuteResult },
    Close,
}

#[derive(Deserialize)]
struct HranaError {
    message: String,
}

#[derive(Deserialize)]
struct ExecuteResult {
    #[serde(default)]
    rows: Vec<Vec<HranaValue>>,
}

/// Values arrive tagged with their SQLite type; `integer` is a decimal string
/// for the same precision reason it is sent as one.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum HranaValue {
    Null,
    Integer {
        value: String,
    },
    Float {
        value: f64,
    },
    Text {
        value: String,
    },
    /// The payload is deliberately dropped: nothing here stores a BLOB, and
    /// the variant exists only so that one would decode instead of failing the
    /// whole response.
    Blob {},
}

impl HranaValue {
    fn as_str(&self) -> Option<&str> {
        match self {
            HranaValue::Text { value } => Some(value),
            _ => None,
        }
    }

    fn as_i64(&self) -> Option<i64> {
        match self {
            HranaValue::Integer { value } => value.parse().ok(),
            HranaValue::Float { value } => Some(*value as i64),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_use_the_tagged_value_encoding() {
        assert_eq!(text("a"), json!({"type": "text", "value": "a"}));
        assert_eq!(
            integer(1_700_000_000_000),
            json!({"type": "integer", "value": "1700000000000"})
        );
        assert_eq!(nullable_text(None), json!({"type": "null"}));
    }

    #[test]
    fn a_select_response_decodes_into_records() {
        let body = json!({
            "baton": null,
            "base_url": null,
            "results": [
                {
                    "type": "ok",
                    "response": {
                        "type": "execute",
                        "result": {
                            "cols": [],
                            "rows": [[
                                {"type": "integer", "value": "1700000000000"},
                                {"type": "text", "value": "webhook"},
                                {"type": "text", "value": "10.0.0.1:5000"},
                                {"type": "text", "value": "cache.purge"},
                                {"type": "text", "value": "npm/vue@3"},
                                {"type": "integer", "value": "1"},
                                {"type": "null"}
                            ]],
                            "affected_row_count": 0,
                            "last_insert_rowid": null
                        }
                    }
                },
                { "type": "ok", "response": { "type": "close" } }
            ]
        });

        let parsed: PipelineResponse = serde_json::from_value(body).expect("a documented response");
        let Step::Ok {
            response: StepResponse::Execute { result },
        } = parsed.results.into_iter().next().unwrap()
        else {
            panic!("the first step is an execute");
        };

        let record = row_to_record(&result.rows[0]).unwrap();
        assert_eq!(record.ts, 1_700_000_000_000);
        assert_eq!(record.actor, Actor::Webhook);
        assert_eq!(record.remote_addr.as_deref(), Some("10.0.0.1:5000"));
        assert_eq!(record.action, "cache.purge");
        assert_eq!(record.target.as_deref(), Some("npm/vue@3"));
        assert!(record.success);
        assert!(record.detail.is_none());
    }

    #[test]
    fn a_rejected_statement_is_reported_as_an_error_step() {
        let body = json!({
            "results": [
                { "type": "error", "error": { "message": "no such table: admin_audit_log" } }
            ]
        });
        let parsed: PipelineResponse = serde_json::from_value(body).unwrap();
        assert!(matches!(parsed.results[0], Step::Error { .. }));
    }

    #[test]
    fn blob_and_float_columns_do_not_break_decoding() {
        let row: Vec<HranaValue> = serde_json::from_value(json!([
            {"type": "float", "value": 1.5},
            {"type": "blob", "base64": "AAEC"},
            {"type": "null"}
        ]))
        .unwrap();
        assert_eq!(row[0].as_i64(), Some(1));
        assert!(row[1].as_str().is_none());
        assert!(row[2].as_i64().is_none());
    }
}
