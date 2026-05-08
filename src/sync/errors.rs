use serde::Serialize;
use serde_json::{json, Value};

/// Stable error codes surfaced in `{error.code}` for agents to switch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    NoWorkspace,
    YamlParse,
    SchemaViolation,
    RenderFailed,
    AuthMissing,
    ToolCallFailed,
    RemoteNotFound,
    StateDb,
    NotImplemented,
}

impl Code {
    pub fn as_str(&self) -> &'static str {
        match self {
            Code::NoWorkspace => "E_NO_WORKSPACE",
            Code::YamlParse => "E_YAML_PARSE",
            Code::SchemaViolation => "E_SCHEMA_VIOLATION",
            Code::RenderFailed => "E_RENDER_FAILED",
            Code::AuthMissing => "E_AUTH_MISSING",
            Code::ToolCallFailed => "E_TOOL_CALL_FAILED",
            Code::RemoteNotFound => "E_REMOTE_NOT_FOUND",
            Code::StateDb => "E_STATE_DB",
            Code::NotImplemented => "E_NOT_IMPLEMENTED",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncError {
    pub code: &'static str,
    pub message: String,
    pub remediation: String,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub details: Value,
}

impl SyncError {
    pub fn new(code: Code, message: impl Into<String>, remediation: impl Into<String>) -> Self {
        Self {
            code: code.as_str(),
            message: message.into(),
            remediation: remediation.into(),
            details: Value::Null,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if !self.remediation.is_empty() {
            write!(f, "\n  -> {}", self.remediation)?;
        }
        Ok(())
    }
}

impl std::error::Error for SyncError {}

/// Emit a successful JSON envelope to stdout.
pub fn print_ok_json(data: Value, pretty: bool) {
    let env = json!({ "ok": true, "data": data });
    let s = if pretty {
        serde_json::to_string_pretty(&env).unwrap_or_else(|_| env.to_string())
    } else {
        env.to_string()
    };
    println!("{s}");
}

/// Emit a failure JSON envelope to stdout.
pub fn print_err_json(err: &SyncError, pretty: bool) {
    let env = json!({ "ok": false, "error": err });
    let s = if pretty {
        serde_json::to_string_pretty(&env).unwrap_or_else(|_| env.to_string())
    } else {
        env.to_string()
    };
    println!("{s}");
}
