use std::io;

use rmcp::model::{CallToolResult, ContentBlock};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::admin::AuditEntry;

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditField {
    Actor,
    Action,
    Result,
}

pub fn no_node(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused)
}

pub fn audit_item(fields: &[AuditField], entry: &AuditEntry) -> serde_json::Value {
    let mut item = serde_json::Map::new();
    for field in fields {
        let (key, value) = match field {
            AuditField::Actor => ("actor", &entry.actor),
            AuditField::Action => ("action", &entry.action),
            AuditField::Result => ("result", &entry.result),
        };
        item.insert(key.to_owned(), json!(value));
    }
    serde_json::Value::Object(item)
}

pub fn tool_success(message: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(message)])
}

pub fn tool_error(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// The answer when the node refuses `what` with the error `code`.
pub fn refused(what: &str, code: &str) -> CallToolResult {
    tool_error(format!("the node refused {what}: {code}"))
}

/// A success with the text of `Ok`, else an error with the text of `Err`.
pub fn tool_result(
    result: Result<impl Into<String>, impl Into<String>>,
) -> CallToolResult {
    match result {
        Ok(message) => tool_success(message),
        Err(message) => tool_error(message),
    }
}
