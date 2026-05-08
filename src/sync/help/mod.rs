use crate::sync::errors::{Code, SyncError};

const WORKFLOW: &str = include_str!("topics/workflow.md");
const SCHEMA_TOPIC: &str = include_str!("topics/schema.md");
const TROUBLESHOOTING: &str = include_str!("topics/troubleshooting.md");
const KLAVIYO: &str = include_str!("topics/klaviyo.md");
const KLAVIYO_TEMPLATE: &str = include_str!("topics/klaviyo_template.md");

pub fn render(topic: &[String]) -> Result<String, SyncError> {
    let joined = topic
        .iter()
        .map(|s| s.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    let body = match joined.as_str() {
        "" | "workflow" => WORKFLOW,
        "schema" => SCHEMA_TOPIC,
        "troubleshooting" => TROUBLESHOOTING,
        "klaviyo" => KLAVIYO,
        "klaviyo template" => KLAVIYO_TEMPLATE,
        _ => {
            return Err(SyncError::new(
                Code::NotImplemented,
                format!("Unknown help topic: '{joined}'"),
                "Valid topics: workflow | schema | troubleshooting | klaviyo | klaviyo template",
            ));
        }
    };
    Ok(body.to_string())
}

