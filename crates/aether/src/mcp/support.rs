#![allow(clippy::redundant_pub_crate)]
use std::io;

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::json;

use super::args::AuditField;
use crate::admin::AuditEntry;

pub(crate) fn no_node(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused)
}

pub(crate) fn audit_item(
    fields: &[AuditField],
    entry: &AuditEntry,
) -> serde_json::Value {
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

pub(crate) fn tool_success(message: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(message)])
}

pub(crate) fn tool_error(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}
