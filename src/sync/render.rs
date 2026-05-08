use crate::sync::errors::{Code, SyncError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderSpec {
    pub render: String,
    #[serde(default)]
    pub command: Vec<String>,
}

pub struct Rendered {
    pub bytes: Vec<u8>,
    pub hash: String,
}

pub fn render(spec: &RenderSpec, cwd: &Path, resource: &str) -> Result<Rendered, SyncError> {
    match spec.render.as_str() {
        "exec" => render_exec(spec, cwd, resource),
        other => Err(SyncError::new(
            Code::RenderFailed,
            format!("Unsupported renderer: {other}"),
            "Only `render: exec` is supported in the prototype.",
        )),
    }
}

fn render_exec(spec: &RenderSpec, cwd: &Path, resource: &str) -> Result<Rendered, SyncError> {
    if spec.command.is_empty() {
        return Err(SyncError::new(
            Code::RenderFailed,
            format!("Resource '{resource}' has empty render.command"),
            "Set render.command to the argv list that produces HTML on stdout.",
        ));
    }
    let (program, args) = spec.command.split_first().unwrap();
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| {
            SyncError::new(
                Code::RenderFailed,
                format!("Failed to spawn `{program}` for resource '{resource}': {e}"),
                format!("Ensure `{program}` is on PATH."),
            )
            .with_details(json!({ "resource": resource, "command": spec.command }))
        })?;
    if !output.status.success() {
        let stderr_tail = tail(&output.stderr, 2000);
        return Err(SyncError::new(
            Code::RenderFailed,
            format!(
                "Rendering resource '{resource}' failed with exit code {}",
                output.status.code().unwrap_or(-1)
            ),
            format!(
                "Run the render command manually to see the error: {}",
                spec.command.join(" ")
            ),
        )
        .with_details(json!({
            "resource": resource,
            "command": spec.command,
            "exit_code": output.status.code(),
            "stderr_tail": stderr_tail,
        })));
    }
    let mut hasher = Sha256::new();
    hasher.update(&output.stdout);
    let hash = format!("{:x}", hasher.finalize());
    Ok(Rendered {
        bytes: output.stdout,
        hash,
    })
}

fn tail(bytes: &[u8], max: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    if s.len() <= max {
        return s.into_owned();
    }
    let start = s.len() - max;
    let mut start = start;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

pub fn combined_hash(yaml_canonical: &[u8], html: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(yaml_canonical);
    hasher.update([0u8]);
    hasher.update(html);
    format!("{:x}", hasher.finalize())
}
