use crate::api::ApiClient;
use crate::sync::errors::{Code, SyncError};
use crate::sync::plan::{Action, ActionKind, Plan};
use crate::sync::providers::klaviyo;
use crate::sync::state::State;

pub struct ApplyOptions {
    pub dry_run: bool,
}

pub struct ApplyReport {
    pub created: usize,
    pub updated: usize,
}

pub fn apply(
    client: &ApiClient,
    state: &State,
    plan: &Plan,
    opts: ApplyOptions,
) -> Result<ApplyReport, SyncError> {
    let mut report = ApplyReport {
        created: 0,
        updated: 0,
    };

    // Apply in deterministic file-name order: creates first, then updates.
    for action in plan.creates.iter().chain(plan.updates.iter()) {
        if opts.dry_run {
            println!(
                "would {}: {}.{}.{}",
                action.kind_action.as_str(),
                action.provider,
                action.kind,
                action.name
            );
            continue;
        }

        let log_id = state.log_apply_start(
            &action.provider,
            &action.kind,
            &action.name,
            action.kind_action.as_str(),
        )?;

        let outcome = execute_action(client, action);

        match &outcome {
            Ok(remote_id) => {
                state.mark_applied(
                    &action.provider,
                    &action.kind,
                    &action.name,
                    Some(remote_id.as_str()),
                    &action.desired_hash,
                )?;
                state.log_apply_finish(log_id, "success", None, Some(remote_id.as_str()))?;
                match action.kind_action {
                    ActionKind::Create => report.created += 1,
                    ActionKind::Update => report.updated += 1,
                    ActionKind::Noop => {}
                }
                println!(
                    "  {} {}.{}.{} (remote_id={})",
                    action.kind_action.as_str(),
                    action.provider,
                    action.kind,
                    action.name,
                    remote_id
                );
            }
            Err(e) => {
                state.log_apply_finish(
                    log_id,
                    "failed",
                    Some(&format!("{}: {}", e.code, e.message)),
                    None,
                )?;
                println!(
                    "  FAILED {}.{}.{}: {}",
                    action.provider, action.kind, action.name, e.message
                );
                return Err(e.clone());
            }
        }
    }

    Ok(report)
}

fn execute_action(client: &ApiClient, action: &Action) -> Result<String, SyncError> {
    match action.provider.as_str() {
        "klaviyo" => match action.kind_action {
            ActionKind::Create => klaviyo::create_template(client, action),
            ActionKind::Update => match klaviyo::update_template(client, action) {
                Ok(id) => Ok(id),
                Err(err) if err.code == Code::RemoteNotFound.as_str() => {
                    klaviyo::create_template(client, action)
                }
                Err(e) => Err(e),
            },
            ActionKind::Noop => Ok(action.remote_id.clone().unwrap_or_default()),
        },
        other => Err(SyncError::new(
            Code::NotImplemented,
            format!("Provider '{other}' has no apply implementation"),
            "",
        )),
    }
}
