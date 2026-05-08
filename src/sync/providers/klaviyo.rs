use crate::api::{ApiClient, ToolCallResponse};
use crate::sync::errors::{Code, SyncError};
use crate::sync::plan::Action;
use crate::sync::render::RenderSpec;
use crate::sync::state::{ResourceRow, State};
use crate::sync::workspace::Workspace;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub const TOOL_CALL_PATH: &str = "/v1/tool-call";

// ─── YAML shape ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateResource {
    /// Must be "template" — denormalized at the top of every file.
    #[serde(default)]
    pub kind: Option<String>,
    /// Human-readable template name (sent to Klaviyo as `attributes.name`).
    pub name: String,
    pub html: RenderSpec,

    // Filled in from filename, not persisted when re-serialized.
    #[serde(skip)]
    pub name_slug: String,
    #[serde(skip)]
    pub source_path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct SerializableTemplate<'a> {
    pub kind: &'static str,
    pub name: &'a str,
    pub html: &'a RenderSpec,
}

impl TemplateResource {
    pub fn with_source(mut self, path: &Path) -> Self {
        self.source_path = path.to_path_buf();
        self.name_slug = slug_from_filename(path);
        if self.kind.is_none() {
            self.kind = Some("template".into());
        }
        self
    }

    pub fn to_serializable(&self) -> SerializableTemplate<'_> {
        SerializableTemplate {
            kind: "template",
            name: &self.name,
            html: &self.html,
        }
    }
}

pub fn slug_from_filename(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.replace('-', "_"))
        .unwrap_or_else(|| "unknown".to_string())
}

pub fn rel_path(root: &Path, abs: &Path) -> String {
    pathdiff(root, abs).unwrap_or_else(|| abs.to_string_lossy().to_string())
}

fn pathdiff(base: &Path, target: &Path) -> Option<String> {
    target
        .strip_prefix(base)
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

// ─── Import ───────────────────────────────────────────────────────────

pub fn import_templates(
    client: &ApiClient,
    ws: &Workspace,
    state: &State,
) -> Result<usize, SyncError> {
    let dir = ws.integrations_dir().join("klaviyo").join("templates");
    fs::create_dir_all(&dir).map_err(|e| {
        SyncError::new(
            Code::StateDb,
            format!("Failed to create {}: {}", dir.display(), e),
            "",
        )
    })?;

    // Best-effort: read superveggie's existing klaviyo-manifest.json if present
    // so imported filenames match existing slugs and we can backlink TSX sources.
    let manifest = read_legacy_manifest(&ws.root);

    let mut count = 0;
    let mut cursor: Option<String> = None;
    loop {
        let mut args = serde_json::Map::new();
        args.insert(
            "fields[template]".into(),
            json!(["name", "html", "editor_type", "updated"]),
        );
        if let Some(c) = &cursor {
            args.insert("page[cursor]".into(), json!(c));
        }
        let body = json!({
            "toolName": "klaviyo_get_templates",
            "args": Value::Object(args),
        });
        let resp = client.post_tool_call(TOOL_CALL_PATH, &body).map_err(|e| {
            SyncError::new(
                Code::ToolCallFailed,
                format!("klaviyo_get_templates failed: {e}"),
                "Check your bisque auth (`bisque doctor`).",
            )
        })?;
        let result = match resp {
            ToolCallResponse::Json(v) => v,
            ToolCallResponse::Binary { .. } => {
                return Err(SyncError::new(
                    Code::ToolCallFailed,
                    "klaviyo_get_templates returned binary data",
                    "Unexpected — file an issue.",
                ))
            }
        };

        let data = result
            .pointer("/result/data")
            .or_else(|| result.pointer("/data/data"))
            .or_else(|| result.pointer("/data"))
            .cloned()
            .unwrap_or(Value::Null);
        let templates = match data.as_array() {
            Some(arr) => arr.clone(),
            None => {
                return Err(SyncError::new(
                    Code::ToolCallFailed,
                    "klaviyo_get_templates: unexpected response shape (expected data array)",
                    "Inspect raw response with `bisque call klaviyo_get_templates`.",
                )
                .with_details(result.clone()));
            }
        };

        for t in templates {
            let remote_id = t
                .pointer("/id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name = t
                .pointer("/attributes/name")
                .and_then(|v| v.as_str())
                .unwrap_or("untitled")
                .to_string();
            if remote_id.is_empty() {
                continue;
            }
            let slug = slug_from_remote_name(&name, &manifest, &remote_id);
            let yaml_path = dir.join(format!("{slug}.yaml"));

            let tsx_hint = manifest
                .as_ref()
                .and_then(|m| find_tsx_for_slug(&ws.root, &slug, m));
            let yaml_body = emit_template_yaml(&name, tsx_hint.as_deref());

            fs::write(&yaml_path, yaml_body).map_err(|e| {
                SyncError::new(
                    Code::StateDb,
                    format!("Failed to write {}: {}", yaml_path.display(), e),
                    "",
                )
            })?;

            // Insert state row with the remote_id. desired_hash is "" because we
            // haven't rendered yet; the first `plan` after import will populate
            // the real hash via an UPDATE or NOOP.
            state.upsert_resource(&ResourceRow {
                provider: "klaviyo".into(),
                kind: "template".into(),
                name: slug.clone(),
                file_path: rel_path(&ws.root, &yaml_path),
                remote_id: Some(remote_id.clone()),
                desired_hash: String::new(),
                applied_hash: None,
                last_applied: None,
            })?;
            count += 1;
        }

        cursor = result
            .pointer("/result/links/next")
            .or_else(|| result.pointer("/data/links/next"))
            .and_then(|v| v.as_str())
            .and_then(extract_cursor);
        if cursor.is_none() {
            break;
        }
    }
    Ok(count)
}

fn extract_cursor(href: &str) -> Option<String> {
    // `next` can be a full URL or just a cursor value. Extract ?page[cursor]= if present.
    if let Some(idx) = href.find("page[cursor]=") {
        let rest = &href[idx + "page[cursor]=".len()..];
        let end = rest.find('&').unwrap_or(rest.len());
        Some(rest[..end].to_string())
    } else {
        Some(href.to_string())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct LegacyManifest {
    #[serde(default)]
    templates: std::collections::HashMap<String, String>,
}

fn read_legacy_manifest(root: &Path) -> Option<LegacyManifest> {
    let p = root.join("functions/emails/klaviyo-manifest.json");
    let raw = fs::read_to_string(p).ok()?;
    serde_json::from_str(&raw).ok()
}

fn slug_from_remote_name(name: &str, manifest: &Option<LegacyManifest>, remote_id: &str) -> String {
    if let Some(m) = manifest {
        for (slug, id) in &m.templates {
            if id == remote_id {
                return slug.clone();
            }
        }
    }
    // Fallback: sanitize the remote name.
    sanitize(name)
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

/// If a slug matches a known react-email source (heuristic: a .tsx under
/// functions/emails/src/emails/ whose stem matches), emit the render.command
/// pointing at it. Otherwise None and the YAML carries a TODO comment.
fn find_tsx_for_slug(root: &Path, slug: &str, _m: &LegacyManifest) -> Option<String> {
    // Heuristic search — not exhaustive, just enough to land a working render command
    // for templates whose source path is predictable.
    let candidates = [
        format!("functions/emails/src/emails/{}.tsx", slug.replace('_', "-")),
        format!("functions/emails/src/emails/{}.tsx", slug),
    ];
    for c in candidates {
        if root.join(&c).is_file() {
            return Some(c);
        }
    }
    None
}

fn emit_template_yaml(name: &str, tsx: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str("kind: template\n");
    out.push_str(&format!("name: {}\n", yaml_quote(name)));
    out.push_str("html:\n");
    out.push_str("  render: exec\n");
    match tsx {
        Some(path) => {
            out.push_str("  command:\n");
            out.push_str("    - bun\n");
            out.push_str("    - functions/emails/scripts/render-one.tsx\n");
            out.push_str("    - --export\n");
            out.push_str("    - default\n");
            out.push_str(&format!("    - {path}\n"));
        }
        None => {
            out.push_str(
                "  # TODO: point render.command at the script that prints this template's HTML on stdout.\n",
            );
            out.push_str("  # Example:\n");
            out.push_str("  #   command:\n");
            out.push_str("  #     - bun\n");
            out.push_str("  #     - functions/emails/scripts/render-one.tsx\n");
            out.push_str("  #     - --export\n");
            out.push_str("  #     - default\n");
            out.push_str("  #     - functions/emails/src/emails/<file>.tsx\n");
            out.push_str("  command: []\n");
        }
    }
    out
}

fn yaml_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        && !s.is_empty()
    {
        s.to_string()
    } else {
        format!(
            "\"{}\"",
            s.replace('\\', "\\\\").replace('"', "\\\"")
        )
    }
}

// ─── Apply (create/update) ────────────────────────────────────────────

pub fn create_template(client: &ApiClient, action: &Action) -> Result<String, SyncError> {
    let html = String::from_utf8_lossy(&action.html).to_string();
    let body = json!({
        "toolName": "klaviyo_create_template",
        "args": {
            "data": {
                "type": "template",
                "attributes": {
                    "name": action.klaviyo_name,
                    "editor_type": "CODE",
                    "html": html,
                }
            }
        }
    });
    let resp = call(client, &body)?;
    let id = resp
        .pointer("/result/data/id")
        .or_else(|| resp.pointer("/data/data/id"))
        .or_else(|| resp.pointer("/data/id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            SyncError::new(
                Code::ToolCallFailed,
                "klaviyo_create_template did not return data.id",
                "Inspect with `bisque call klaviyo_create_template`.",
            )
            .with_details(resp.clone())
        })?
        .to_string();
    Ok(id)
}

pub fn update_template(client: &ApiClient, action: &Action) -> Result<String, SyncError> {
    let remote_id = action.remote_id.clone().ok_or_else(|| {
        SyncError::new(
            Code::RemoteNotFound,
            format!("No remote_id for {}.{}", action.kind, action.name),
            "Run `bisque-sync import klaviyo templates` first.",
        )
    })?;
    let html = String::from_utf8_lossy(&action.html).to_string();
    let body = json!({
        "toolName": "klaviyo_update_template",
        "args": {
            "id": remote_id,
            "data": {
                "type": "template",
                "id": remote_id,
                "attributes": {
                    "name": action.klaviyo_name,
                    "html": html,
                }
            }
        }
    });
    let resp = match call(client, &body) {
        Ok(v) => v,
        Err(e) => {
            // Heuristic: if the backend surfaces a 404 in the message, signal
            // RemoteNotFound so apply.rs can fall back to create.
            if e.message.contains("404") || e.message.to_lowercase().contains("not found") {
                return Err(SyncError::new(
                    Code::RemoteNotFound,
                    e.message.clone(),
                    "Will fall back to create.",
                ));
            }
            return Err(e);
        }
    };
    // update returns either updated data.id or status succeeded.
    let id = resp
        .pointer("/result/data/id")
        .or_else(|| resp.pointer("/data/data/id"))
        .and_then(|v| v.as_str())
        .unwrap_or(remote_id.as_str())
        .to_string();
    Ok(id)
}

fn call(client: &ApiClient, body: &Value) -> Result<Value, SyncError> {
    let resp = client.post_tool_call(TOOL_CALL_PATH, body).map_err(|e| {
        SyncError::new(
            Code::ToolCallFailed,
            format!("{e}"),
            "Check auth and provider connection.",
        )
    })?;
    match resp {
        ToolCallResponse::Json(v) => {
            if let Some(status) = v.pointer("/status").and_then(|s| s.as_str()) {
                if status != "succeeded" && status != "ok" {
                    return Err(SyncError::new(
                        Code::ToolCallFailed,
                        format!("tool call status={status}"),
                        "Inspect the details field.",
                    )
                    .with_details(v.clone()));
                }
            }
            Ok(v)
        }
        ToolCallResponse::Binary { .. } => Err(SyncError::new(
            Code::ToolCallFailed,
            "klaviyo tool call returned binary response",
            "",
        )),
    }
}

// ─── Schema ───────────────────────────────────────────────────────────

pub const TEMPLATE_SCHEMA: &str = include_str!("klaviyo_schemas/template.schema.json");

pub fn schema_for(kind: &str) -> Option<&'static str> {
    match kind {
        "template" => Some(TEMPLATE_SCHEMA),
        _ => None,
    }
}

pub fn supported_kinds() -> &'static [&'static str] {
    &["template"]
}
