//! Tx and its companions implement the §3.5 transaction handle: the
//! txn_id is a first-class field here — it never leaks as a bare tool
//! argument that a caller threads between call sites. The four txn_* tools
//! are reachable only through Tx methods. Mirrors the Go SDK's
//! crates/sdk/go/transactions.go.

use crate::client::Client;
use crate::error::{Error, McpError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Tx is a staged write handle: Begin on the Client, Execute stages ops,
/// Commit or Rollback closes it. A closed handle refuses further use with
/// INVALID_ARGUMENT.
#[derive(Debug)]
pub struct Tx {
    c: Client,
    id: String,
    done: bool,
}

/// StagedOp is one staged write (the txn_stage op shape).
#[derive(Debug, Clone, Default, Serialize)]
pub struct StagedOp {
    pub action: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub type_name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub koid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<Map<String, Value>>,
}

/// CommitResult is the txn_commit outcome: the staged write results and
/// whether this commit was a retry of an already-committed txn_id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitResult {
    #[serde(default)]
    pub results: Vec<Value>,
    #[serde(default)]
    pub deduped: bool,
}

impl Client {
    /// Opens a transaction. With no id a random 32-hex id is generated;
    /// passing one retries the same begin idempotently (P5-M20).
    pub async fn begin(&self, txn_id: Option<&str>) -> Result<Tx, Error> {
        let id = match txn_id {
            Some(id) => id.to_string(),
            None => {
                let mut b = [0u8; 16];
                getrandom::getrandom(&mut b).map_err(|e| Error::Kernel(format!("txn id: {e}")))?;
                b.iter().map(|x| format!("{x:02x}")).collect()
            }
        };
        let mut args = Map::new();
        args.insert("txn_id".into(), Value::String(id.clone()));
        self.call_tool("txn_begin", Some(Value::Object(args)))
            .await?;
        Ok(Tx {
            c: self.clone(),
            id,
            done: false,
        })
    }
}

impl Tx {
    /// The txn_id (for retries — commit dedupes by it, P5-M20).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Stages one write.
    pub async fn execute(&mut self, op: StagedOp) -> Result<(), Error> {
        let mut args = Map::new();
        args.insert("txn_id".into(), Value::String(self.id.clone()));
        args.insert("op".into(), serde_json::to_value(op).map_err(Error::Json)?);
        self.step("txn_stage", args).await?;
        Ok(())
    }

    /// Applies the staged writes and closes the handle. A commit that
    /// errors leaves the handle open: the server dedupes by txn_id, so the
    /// caller may retry Commit or Begin with the same id (P5-M20).
    pub async fn commit(&mut self) -> Result<CommitResult, Error> {
        let mut args = Map::new();
        args.insert("txn_id".into(), Value::String(self.id.clone()));
        let raw = self.step("txn_commit", args).await?;
        serde_json::from_value(raw).map_err(Error::Json)
    }

    /// Discards the staged writes and closes the handle.
    pub async fn rollback(&mut self) -> Result<(), Error> {
        let mut args = Map::new();
        args.insert("txn_id".into(), Value::String(self.id.clone()));
        self.step("txn_rollback", args).await?;
        Ok(())
    }

    async fn step(&mut self, name: &str, args: Map<String, Value>) -> Result<Value, Error> {
        if self.done {
            return Err(Error::Mcp(McpError::invalid_argument(format!(
                "transaction {} is closed",
                self.id
            ))));
        }
        let raw = self.c.call_tool(name, Some(Value::Object(args))).await?;
        if name == "txn_commit" || name == "txn_rollback" {
            self.done = true;
        }
        Ok(raw)
    }
}
