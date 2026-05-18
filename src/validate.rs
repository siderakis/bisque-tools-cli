// Local JSON Schema validation for `bisque call`.
//
// Looks up the tool's schema in synced skill directories
// (`~/.claude/skills/bisque-*/tools.json` and `~/.codex/skills/bisque-*/tools.json`)
// and validates the user-supplied args against it BEFORE hitting the proxy,
// so wrong-arg-name mistakes get caught at iteration time with a useful hint.
//
// Falls through silently when no local schema is found — the catalog can be
// ahead of the synced skills, and we don't want to block calls in that case.

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, Clone, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    #[serde(default)]
    pub parameters: ParameterSchema,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ParameterSchema {
    #[serde(default)]
    pub properties: serde_json::Map<String, Value>,
    #[serde(default)]
    pub required: Vec<String>,
}

/// Aggregated validation errors for a single call. Empty `issues` means OK.
#[derive(Debug, Default)]
pub struct ValidationReport {
    pub tool_name: String,
    pub passed_fields: Vec<String>,
    pub issues: Vec<Issue>,
}

#[derive(Debug)]
pub enum Issue {
    MissingRequired {
        field: String,
        suggestion: Option<String>,
    },
    UnknownField {
        field: String,
        suggestion: Option<String>,
    },
    TypeMismatch {
        field: String,
        expected: String,
        got: String,
    },
}

impl ValidationReport {
    pub fn ok(&self) -> bool {
        self.issues.is_empty()
    }

    /// Render as a multi-line error message intended for stderr.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for issue in &self.issues {
            match issue {
                Issue::MissingRequired { field, suggestion } => {
                    out.push_str(&format!(
                        "error: missing required field \"{}\"\n",
                        field
                    ));
                    out.push_str(&format!(
                        "       you passed: {:?}\n",
                        self.passed_fields
                    ));
                    if let Some(s) = suggestion {
                        out.push_str(&format!(
                            "       did you mean: {}? (snake_case → camelCase match)\n",
                            s
                        ));
                    }
                }
                Issue::UnknownField { field, suggestion } => {
                    out.push_str(&format!(
                        "error: unknown field \"{}\" for tool \"{}\"\n",
                        field, self.tool_name
                    ));
                    if let Some(s) = suggestion {
                        out.push_str(&format!(
                            "       did you mean: {}? (snake_case → camelCase match)\n",
                            s
                        ));
                    }
                }
                Issue::TypeMismatch {
                    field,
                    expected,
                    got,
                } => {
                    out.push_str(&format!(
                        "error: field \"{}\" expected type {}, got {}\n",
                        field, expected, got
                    ));
                }
            }
        }
        out.push_str(&format!(
            "       (run with --skip-schema-check to bypass)\n"
        ));
        out
    }
}

/// Find the schema for `tool_name` in any synced skill directory.
/// Returns `None` if no local schema is found (caller should proceed without
/// validation).
pub fn find_tool_schema(tool_name: &str) -> Option<ToolSchema> {
    let cache = load_all_schemas();
    cache.get(tool_name).cloned()
}

/// Validate `args` (must be a JSON object) against `schema`.
pub fn validate_args(tool_name: &str, args: &Value, schema: &ToolSchema) -> ValidationReport {
    let mut report = ValidationReport {
        tool_name: tool_name.to_string(),
        ..Default::default()
    };

    let obj = match args.as_object() {
        Some(o) => o,
        None => return report,
    };

    let passed_keys: Vec<String> = obj.keys().cloned().collect();
    report.passed_fields = passed_keys.clone();

    let schema_keys: Vec<&str> = schema
        .parameters
        .properties
        .keys()
        .map(String::as_str)
        .collect();

    // Missing required fields. If a required field is missing but a similar
    // user-passed key exists (e.g. `account_id` when the schema wants
    // `adAccountId`), surface that as the suggestion.
    for req in &schema.parameters.required {
        if obj.contains_key(req) {
            continue;
        }
        let suggestion = best_match(req, passed_keys.iter().map(String::as_str));
        report.issues.push(Issue::MissingRequired {
            field: req.clone(),
            suggestion,
        });
    }

    // Unknown fields. For each user-passed key not in the schema, suggest the
    // closest canonical schema name.
    for key in &passed_keys {
        if schema.parameters.properties.contains_key(key) {
            continue;
        }
        let suggestion = best_match(key, schema_keys.iter().copied());
        report.issues.push(Issue::UnknownField {
            field: key.clone(),
            suggestion,
        });
    }

    // Light type check on present, recognized fields.
    for (key, value) in obj {
        let Some(prop) = schema.parameters.properties.get(key) else {
            continue;
        };
        let Some(expected_ty) = prop.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        if !type_matches(expected_ty, value) {
            report.issues.push(Issue::TypeMismatch {
                field: key.clone(),
                expected: expected_ty.to_string(),
                got: value_type_name(value).to_string(),
            });
        }
    }

    report
}

fn type_matches(expected: &str, value: &Value) -> bool {
    match expected {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "null" => value.is_null(),
        // Unknown/composite schemas — don't fail on it.
        _ => true,
    }
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Strip `_` and `-`, lowercase. Lets us match `account_id` ↔ `accountId`,
/// `ad-account-id` ↔ `adAccountId`, etc.
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Pick the most likely "did you mean" candidate among `candidates` for `key`.
/// Strategy, in order of priority:
///   1. Exact normalized match (`account_id` ↔ `accountId`).
///   2. Substring containment after normalization (`account_id` ↔ `adAccountId`).
///      Requires the shorter normalized form to be ≥ 4 chars to avoid noise.
///   3. Otherwise no suggestion.
fn best_match<'a>(key: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let norm_key = normalize(key);
    let mut exact: Option<&str> = None;
    let mut substring: Option<&str> = None;

    for cand in candidates {
        let norm_cand = normalize(cand);
        if norm_cand == norm_key {
            exact = Some(cand);
            break;
        }
        if substring.is_none() {
            let (shorter, longer) = if norm_key.len() <= norm_cand.len() {
                (&norm_key, &norm_cand)
            } else {
                (&norm_cand, &norm_key)
            };
            if shorter.len() >= 4 && longer.contains(shorter.as_str()) {
                substring = Some(cand);
            }
        }
    }

    exact.or(substring).map(|s| s.to_string())
}

// ── Schema cache ────────────────────────────────────────────────────
//
// Build a map { tool_name -> ToolSchema } by scanning every
// `~/.claude/skills/bisque-*/tools.json` (and the codex equivalent) on the
// first lookup, then memoize for the rest of the process.

static SCHEMA_CACHE: OnceLock<std::collections::HashMap<String, ToolSchema>> = OnceLock::new();

fn load_all_schemas() -> &'static std::collections::HashMap<String, ToolSchema> {
    SCHEMA_CACHE.get_or_init(|| {
        let mut map = std::collections::HashMap::new();
        for root in skill_roots() {
            let Ok(entries) = fs::read_dir(&root) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(ft) = entry.file_type() else { continue };
                if !ft.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.starts_with("bisque-") {
                    continue;
                }
                let tools_path = entry.path().join("tools.json");
                let Ok(content) = fs::read_to_string(&tools_path) else {
                    continue;
                };
                let Ok(tools) = serde_json::from_str::<Vec<ToolSchema>>(&content) else {
                    continue;
                };
                for tool in tools {
                    map.entry(tool.name.clone()).or_insert(tool);
                }
            }
        }
        map
    })
}

fn skill_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = dirs::home_dir() {
        let claude = home.join(".claude").join("skills");
        if claude.is_dir() {
            roots.push(claude);
        }
        let codex_home = std::env::var("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".codex"));
        let codex = codex_home.join("skills");
        if codex.is_dir() {
            roots.push(codex);
        }
    }
    roots
}

/// Convenience: validate `args` against the local schema for `tool_name`.
/// Returns `Ok(())` when validation passes OR when no schema is found.
/// Returns `Err(_)` with a rendered, human-readable message otherwise.
pub fn validate_call(tool_name: &str, args: &Value) -> Result<(), String> {
    let Some(schema) = find_tool_schema(tool_name) else {
        return Ok(());
    };
    let report = validate_args(tool_name, args, &schema);
    if report.ok() {
        Ok(())
    } else {
        Err(report.render())
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema_meta_ads_insights() -> ToolSchema {
        serde_json::from_value(json!({
            "name": "meta_ads_get_account_insights",
            "parameters": {
                "type": "object",
                "properties": {
                    "adAccountId": { "type": "string" },
                    "date_preset": { "type": "string" },
                    "limit": { "type": "integer" }
                },
                "required": ["adAccountId"]
            }
        }))
        .unwrap()
    }

    #[test]
    fn passes_when_all_required_present_and_camel_case_matches() {
        let s = schema_meta_ads_insights();
        let args = json!({"adAccountId": "act_x", "date_preset": "yesterday"});
        let report = validate_args("meta_ads_get_account_insights", &args, &s);
        assert!(report.ok(), "expected ok, got {:?}", report.issues);
    }

    #[test]
    fn flags_missing_required_field() {
        let s = schema_meta_ads_insights();
        let args = json!({"date_preset": "yesterday"});
        let report = validate_args("meta_ads_get_account_insights", &args, &s);
        assert!(!report.ok());
        assert!(matches!(
            report.issues[0],
            Issue::MissingRequired { ref field, .. } if field == "adAccountId"
        ));
    }

    #[test]
    fn flags_wrong_case_and_suggests_canonical_name() {
        let s = schema_meta_ads_insights();
        let args = json!({"account_id": "act_x", "date_preset": "yesterday"});
        let report = validate_args("meta_ads_get_account_insights", &args, &s);
        assert!(!report.ok());

        // Should flag both "missing required adAccountId" with suggestion
        // "account_id", AND "unknown field account_id" with suggestion
        // "adAccountId".
        let missing = report
            .issues
            .iter()
            .find_map(|i| match i {
                Issue::MissingRequired { field, suggestion } => Some((field, suggestion)),
                _ => None,
            })
            .expect("missing-required issue");
        assert_eq!(missing.0, "adAccountId");
        assert_eq!(missing.1.as_deref(), Some("account_id"));

        let unknown = report
            .issues
            .iter()
            .find_map(|i| match i {
                Issue::UnknownField { field, suggestion } => Some((field, suggestion)),
                _ => None,
            })
            .expect("unknown-field issue");
        assert_eq!(unknown.0, "account_id");
        assert_eq!(unknown.1.as_deref(), Some("adAccountId"));
    }

    #[test]
    fn unknown_field_with_no_close_match_has_no_suggestion() {
        let s = schema_meta_ads_insights();
        let args = json!({"adAccountId": "act_x", "totally_made_up": 1});
        let report = validate_args("meta_ads_get_account_insights", &args, &s);
        let unknown = report
            .issues
            .iter()
            .find_map(|i| match i {
                Issue::UnknownField { field, suggestion } => Some((field, suggestion)),
                _ => None,
            })
            .expect("unknown-field issue");
        assert_eq!(unknown.0, "totally_made_up");
        assert_eq!(unknown.1.as_deref(), None);
    }

    #[test]
    fn no_local_schema_falls_through() {
        // Going through the public entry point: an unknown tool name should
        // produce Ok(()) regardless of args.
        let res = validate_call("__no_such_tool_anywhere__", &json!({"foo": "bar"}));
        assert!(res.is_ok());
    }

    #[test]
    fn type_mismatch_flagged() {
        let s = schema_meta_ads_insights();
        let args = json!({"adAccountId": "act_x", "limit": "twenty-five"});
        let report = validate_args("meta_ads_get_account_insights", &args, &s);
        assert!(report
            .issues
            .iter()
            .any(|i| matches!(i, Issue::TypeMismatch { field, .. } if field == "limit")));
    }

    #[test]
    fn normalize_matches_snake_camel_kebab() {
        assert_eq!(normalize("account_id"), normalize("accountId"));
        assert_eq!(normalize("ad-account-id"), normalize("adAccountId"));
        assert_eq!(normalize("AdAccountId"), normalize("adAccountId"));
    }
}
