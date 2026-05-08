use crate::sync::errors::{Code, SyncError};
use crate::sync::providers::klaviyo::{self, TemplateResource};
use crate::sync::render::{combined_hash, render};
use crate::sync::state::State;
use crate::sync::workspace::Workspace;

#[derive(Debug, Clone)]
pub struct Action {
    pub provider: String,
    pub kind: String,
    pub name: String,
    pub file_path: String,
    pub remote_id: Option<String>,
    pub desired_hash: String,
    pub html: Vec<u8>,
    pub klaviyo_name: String,
    pub kind_action: ActionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Create,
    Update,
    Noop,
}

impl ActionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionKind::Create => "create",
            ActionKind::Update => "update",
            ActionKind::Noop => "noop",
        }
    }
}

#[derive(Debug, Default)]
pub struct Plan {
    pub creates: Vec<Action>,
    pub updates: Vec<Action>,
    pub noops: Vec<Action>,
}

impl Plan {
    pub fn has_pending(&self) -> bool {
        !self.creates.is_empty() || !self.updates.is_empty()
    }
}

pub fn build_plan(ws: &Workspace, state: &State) -> Result<Plan, SyncError> {
    let mut plan = Plan::default();
    let providers = ws.providers().map_err(|e| {
        SyncError::new(
            Code::YamlParse,
            format!("Failed to discover providers: {e}"),
            "Check integrations/<provider>/provider.yaml files.",
        )
    })?;

    for provider in providers {
        match provider.provider.as_str() {
            "klaviyo" => {
                plan_klaviyo(ws, state, &provider, &mut plan)?;
            }
            other => {
                return Err(SyncError::new(
                    Code::NotImplemented,
                    format!("Provider '{other}' is not supported in the prototype"),
                    "Only `klaviyo` is wired up in the MVP.",
                ));
            }
        }
    }
    Ok(plan)
}

fn plan_klaviyo(
    ws: &Workspace,
    state: &State,
    provider: &crate::sync::workspace::ProviderConfig,
    plan: &mut Plan,
) -> Result<(), SyncError> {
    let files = provider.list_resource_files("template").map_err(|e| {
        SyncError::new(
            Code::YamlParse,
            format!("Failed to list klaviyo template files: {e}"),
            "",
        )
    })?;
    for path in files {
        let raw = std::fs::read_to_string(&path).map_err(|e| {
            SyncError::new(
                Code::YamlParse,
                format!("Failed to read {}: {}", path.display(), e),
                "",
            )
        })?;
        let resource: TemplateResource = serde_yaml::from_str(&raw).map_err(|e| {
            SyncError::new(
                Code::YamlParse,
                format!("Failed to parse {}: {}", path.display(), e),
                "Validate shape with `bisque-sync schema klaviyo template`.",
            )
        })?;
        let resource = resource.with_source(&path);
        let rendered = render(&resource.html, &ws.root, &resource.name_slug)?;
        // Canonicalize the YAML spec by re-serializing.
        let canonical =
            serde_yaml::to_string(&resource.to_serializable()).unwrap_or_else(|_| raw.clone());
        let desired_hash = combined_hash(canonical.as_bytes(), &rendered.bytes);

        let existing = state.get_resource("klaviyo", "template", &resource.name_slug)?;
        let remote_id = existing.as_ref().and_then(|r| r.remote_id.clone());
        let action_kind = match &existing {
            None => ActionKind::Create,
            Some(row) => match row.applied_hash.as_deref() {
                Some(applied) if applied == desired_hash => ActionKind::Noop,
                _ => {
                    // If we have a remote_id from import we still want UPDATE not CREATE.
                    if row.remote_id.is_some() {
                        ActionKind::Update
                    } else {
                        ActionKind::Create
                    }
                }
            },
        };

        let action = Action {
            provider: "klaviyo".into(),
            kind: "template".into(),
            name: resource.name_slug.clone(),
            file_path: klaviyo::rel_path(&ws.root, &path),
            remote_id,
            desired_hash,
            html: rendered.bytes,
            klaviyo_name: resource.name.clone(),
            kind_action: action_kind,
        };
        match action_kind {
            ActionKind::Create => plan.creates.push(action),
            ActionKind::Update => plan.updates.push(action),
            ActionKind::Noop => plan.noops.push(action),
        }
    }
    Ok(())
}
