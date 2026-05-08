use crate::api::ApiClient;
use crate::config::{self, BisqueProfile};
use crate::sync::apply::{apply as run_apply, ApplyOptions};
use crate::sync::errors::{print_err_json, print_ok_json, Code, SyncError};
use crate::sync::plan::{build_plan, Plan};
use crate::sync::providers::klaviyo;
use crate::sync::render::render as run_render;
use crate::sync::state::State;
use crate::sync::workspace::{load_workspace, Workspace, INTEGRATIONS_DIR, WORKSPACE_MARKER};
use crate::{SyncCli, SyncCommand};
use anyhow::Result;
use serde_json::{json, Value};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::Path;

pub fn run(cli: SyncCli) -> Result<()> {
    let json_mode = cli.json;
    let result: std::result::Result<Value, SyncError> = match &cli.command {
        SyncCommand::Init { no_claude_md } => cmd_init(*no_claude_md, json_mode),
        SyncCommand::Import { provider, kind } => {
            cmd_import(&cli, provider, kind.as_deref(), json_mode)
        }
        SyncCommand::Plan => cmd_plan(json_mode),
        SyncCommand::Apply {
            dry_run,
            auto_approve,
        } => cmd_apply(&cli, *dry_run, *auto_approve, json_mode),
        SyncCommand::Render { resource } => cmd_render(resource, json_mode),
        SyncCommand::Explain => cmd_explain(json_mode),
        SyncCommand::Ls { provider, kind } => {
            cmd_ls(provider.as_deref(), kind.as_deref(), json_mode)
        }
        SyncCommand::Schema { provider, kind } => cmd_schema(provider, kind.as_deref(), json_mode),
        SyncCommand::Doctor => cmd_doctor(&cli, json_mode),
        SyncCommand::Help { topic } => cmd_help(topic),
        SyncCommand::Mcp => Err(SyncError::new(
            Code::NotImplemented,
            "bisque-sync mcp is reserved but not implemented in the prototype.",
            "Use the CLI verbs (plan/apply/explain/schema/ls) for now.",
        )),
    };

    match result {
        Ok(data) => {
            if json_mode {
                print_ok_json(data, true);
            } else if let Some(s) = data.as_str() {
                // Help topics return a plain string; print it verbatim in human mode.
                println!("{s}");
            }
            // For structured data in human mode, each cmd_* is responsible for
            // printing its own human view; we stay silent here to avoid a
            // trailing JSON dump.
            Ok(())
        }
        Err(err) => {
            if json_mode {
                print_err_json(&err, true);
                std::process::exit(1);
            } else {
                eprintln!("{err}");
                std::process::exit(1);
            }
        }
    }
}

// ─── init ─────────────────────────────────────────────────────────────

fn cmd_init(no_claude_md: bool, json_mode: bool) -> Result<Value, SyncError> {
    let cwd = std::env::current_dir().map_err(|e| {
        SyncError::new(
            Code::NoWorkspace,
            format!("Could not read current directory: {e}"),
            "Run from inside a directory that should become the workspace root.",
        )
    })?;

    let bisque_yaml = cwd.join(WORKSPACE_MARKER);
    let created_yaml = if !bisque_yaml.exists() {
        let name = cwd
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("workspace");
        let body = format!("version: 1\nname: {name}\n");
        write_text(&bisque_yaml, &body)?;
        true
    } else {
        false
    };

    let state_dir = cwd.join(".bisque");
    fs::create_dir_all(&state_dir).map_err(|e| {
        SyncError::new(
            Code::StateDb,
            format!("Failed to create .bisque/: {e}"),
            "",
        )
    })?;

    // .gitignore — append `.bisque/state.db` if not already present.
    let gitignore = cwd.join(".gitignore");
    let existing = fs::read_to_string(&gitignore).unwrap_or_default();
    if !existing.lines().any(|l| l.trim() == ".bisque/state.db" || l.trim() == ".bisque/") {
        let mut out = existing;
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(".bisque/state.db\n");
        write_text(&gitignore, &out)?;
    }

    // integrations/ placeholder so agents can orient quickly.
    fs::create_dir_all(cwd.join(INTEGRATIONS_DIR)).ok();

    let mut wrote_claude = false;
    if !no_claude_md {
        let claude = cwd.join("CLAUDE.md");
        let stanza = claude_md_stanza();
        let existing = fs::read_to_string(&claude).unwrap_or_default();
        if !existing.contains("## bisque-sync") {
            let joined = if existing.is_empty() {
                stanza.clone()
            } else {
                let mut s = existing;
                if !s.ends_with('\n') {
                    s.push('\n');
                }
                s.push('\n');
                s.push_str(&stanza);
                s
            };
            write_text(&claude, &joined)?;
            wrote_claude = true;
        }
    }

    if !json_mode {
        println!("Initialized bisque-sync workspace at {}", cwd.display());
        if created_yaml {
            println!("  + bisque.yaml");
        } else {
            println!("  · bisque.yaml (already existed)");
        }
        println!("  + .bisque/ (state dir)");
        if wrote_claude {
            println!("  + CLAUDE.md stanza");
        }
        println!("\nNext: bisque-sync import klaviyo templates");
    }
    Ok(json!({
        "workspace_root": cwd.to_string_lossy(),
        "created_bisque_yaml": created_yaml,
        "wrote_claude_md": wrote_claude,
        "next": "bisque-sync import klaviyo templates",
    }))
}

fn claude_md_stanza() -> String {
    r#"## bisque-sync

This workspace uses bisque-sync for declarative SaaS state management.

- Desired state: `integrations/<provider>/<kind>/*.yaml` — edit freely.
- Source content referenced by YAML (e.g. TSX templates): edit freely.
- Runtime state: `.bisque/state.db` — never edit directly; managed by CLI.
- Workflow: edit YAML or referenced sources → `bisque-sync plan` → `bisque-sync apply`.
- Orient: `bisque-sync explain` prints workspace state + next suggested command.
- Validate before writing YAML: `bisque-sync schema <provider> <kind>`.
- All commands accept `--json` for structured output.
"#
    .to_string()
}

fn write_text(path: &Path, body: &str) -> Result<(), SyncError> {
    fs::write(path, body).map_err(|e| {
        SyncError::new(
            Code::StateDb,
            format!("Failed to write {}: {}", path.display(), e),
            "",
        )
    })
}

// ─── explain ──────────────────────────────────────────────────────────

fn cmd_explain(json_mode: bool) -> Result<Value, SyncError> {
    let ws = load_workspace()?;
    let state = State::open(&ws.state_db_path())?;
    let providers = ws
        .providers()
        .map_err(|e| SyncError::new(Code::YamlParse, format!("{e}"), ""))?;
    let row_count = state.count_resources()?;

    // Count resource files on disk per provider/kind for orientation
    let mut provider_summaries = Vec::new();
    for p in &providers {
        let mut kinds = Vec::new();
        for kind in klaviyo_kinds_for(&p.provider) {
            let files = p.list_resource_files(kind).unwrap_or_default();
            kinds.push(json!({
                "kind": kind,
                "files_on_disk": files.len(),
            }));
        }
        provider_summaries.push(json!({
            "provider": p.provider,
            "kinds": kinds,
        }));
    }

    let next_step = if row_count == 0 {
        "Run `bisque-sync import klaviyo templates` to bootstrap state."
    } else {
        "Run `bisque-sync plan` to see pending changes."
    };

    if !json_mode {
        println!("Workspace: {}", ws.root.display());
        if let Some(name) = &ws.manifest.name {
            println!("Name:      {name}");
        }
        println!("State DB:  {} rows", row_count);
        if providers.is_empty() {
            println!("Providers: (none configured under integrations/)");
        } else {
            println!("Providers:");
            for p in &providers {
                let files: usize = klaviyo_kinds_for(&p.provider)
                    .iter()
                    .map(|k| p.list_resource_files(k).map(|v| v.len()).unwrap_or(0))
                    .sum();
                println!("  - {} ({} resource file(s))", p.provider, files);
            }
        }
        println!("\nNext: {next_step}");
    }

    Ok(json!({
        "workspace_root": ws.root.to_string_lossy(),
        "workspace_name": ws.manifest.name,
        "providers": provider_summaries,
        "state_db": {
            "path": ws.state_db_path().to_string_lossy(),
            "row_count": row_count,
        },
        "next": next_step,
    }))
}

fn klaviyo_kinds_for(provider: &str) -> &'static [&'static str] {
    match provider {
        "klaviyo" => klaviyo::supported_kinds(),
        _ => &[],
    }
}

// ─── schema ───────────────────────────────────────────────────────────

fn cmd_schema(provider: &str, kind: Option<&str>, json_mode: bool) -> Result<Value, SyncError> {
    match provider {
        "klaviyo" => {
            let Some(kind) = kind else {
                let kinds = klaviyo::supported_kinds();
                if !json_mode {
                    println!("klaviyo kinds:");
                    for k in kinds {
                        println!("  - {k}");
                    }
                }
                return Ok(json!({
                    "provider": "klaviyo",
                    "kinds": kinds,
                }));
            };
            match klaviyo::schema_for(kind) {
                Some(s) => {
                    let v: Value = serde_json::from_str(s).map_err(|e| {
                        SyncError::new(
                            Code::SchemaViolation,
                            format!("Embedded schema is invalid JSON: {e}"),
                            "Rebuild the CLI from source.",
                        )
                    })?;
                    if !json_mode {
                        // Schema is a JSON document either way — pretty-print it.
                        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
                    }
                    Ok(v)
                }
                None => Err(SyncError::new(
                    Code::NotImplemented,
                    format!("Unknown klaviyo kind: '{kind}'"),
                    "Supported kinds: template",
                )),
            }
        }
        other => Err(SyncError::new(
            Code::NotImplemented,
            format!("Unknown provider: '{other}'"),
            "Only klaviyo is supported in the prototype.",
        )),
    }
}

// ─── ls ───────────────────────────────────────────────────────────────

fn cmd_ls(
    provider: Option<&str>,
    kind: Option<&str>,
    json_mode: bool,
) -> Result<Value, SyncError> {
    let ws = load_workspace()?;
    let state = State::open(&ws.state_db_path())?;
    let rows = state.list_resources(provider, kind)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "provider": r.provider,
                "kind": r.kind,
                "name": r.name,
                "file_path": r.file_path,
                "remote_id": r.remote_id,
                "last_applied": r.last_applied,
                "has_applied_hash": r.applied_hash.is_some(),
            })
        })
        .collect();
    if !json_mode {
        if rows.is_empty() {
            println!("(no managed resources; run `bisque-sync import klaviyo templates`)");
        } else {
            println!(
                "{:<10} {:<10} {:<40} {:<14} {}",
                "PROVIDER", "KIND", "NAME", "REMOTE_ID", "STATUS"
            );
            for r in &rows {
                let status = if r.applied_hash.is_some() {
                    "applied"
                } else if r.remote_id.is_some() {
                    "imported"
                } else {
                    "pending"
                };
                println!(
                    "{:<10} {:<10} {:<40} {:<14} {}",
                    r.provider,
                    r.kind,
                    truncate(&r.name, 40),
                    r.remote_id.clone().unwrap_or_else(|| "—".into()),
                    status
                );
            }
        }
    }
    Ok(json!({ "count": items.len(), "resources": items }))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

// ─── plan ─────────────────────────────────────────────────────────────

fn cmd_plan(json_mode: bool) -> Result<Value, SyncError> {
    let ws = load_workspace()?;
    let state = State::open(&ws.state_db_path())?;
    let plan = build_plan(&ws, &state)?;
    if !json_mode {
        print_plan_human(&plan);
    }
    Ok(plan_to_json(&plan))
}

fn print_plan_human(plan: &Plan) {
    println!(
        "Plan: {} create, {} update, {} noop",
        plan.creates.len(),
        plan.updates.len(),
        plan.noops.len()
    );
    for a in &plan.creates {
        println!("  CREATE  {}.{}.{}", a.provider, a.kind, a.name);
    }
    for a in &plan.updates {
        println!("  UPDATE  {}.{}.{}", a.provider, a.kind, a.name);
    }
    if plan.has_pending() {
        println!("\nRun `bisque-sync apply` to push these changes.");
    } else if !plan.noops.is_empty() {
        println!("\nAll resources are in sync.");
    }
}

fn plan_to_json(plan: &Plan) -> Value {
    let fmt = |acts: &Vec<crate::sync::plan::Action>, action: &str| -> Vec<Value> {
        acts.iter()
            .map(|a| {
                json!({
                    "provider": a.provider,
                    "kind": a.kind,
                    "name": a.name,
                    "action": action,
                    "file_path": a.file_path,
                    "remote_id": a.remote_id,
                    "desired_hash": a.desired_hash,
                })
            })
            .collect()
    };
    json!({
        "summary": {
            "creates": plan.creates.len(),
            "updates": plan.updates.len(),
            "noops": plan.noops.len(),
        },
        "actions": {
            "create": fmt(&plan.creates, "create"),
            "update": fmt(&plan.updates, "update"),
            "noop": fmt(&plan.noops, "noop"),
        },
    })
}

// ─── apply ────────────────────────────────────────────────────────────

fn cmd_apply(
    cli: &SyncCli,
    dry_run: bool,
    auto_approve: bool,
    json_mode: bool,
) -> Result<Value, SyncError> {
    let ws = load_workspace()?;
    let state = State::open(&ws.state_db_path())?;
    let plan = build_plan(&ws, &state)?;
    if !plan.has_pending() {
        return Ok(json!({ "nothing_to_apply": true }));
    }
    if !dry_run && !auto_approve {
        if !io::stdin().is_terminal() {
            return Err(SyncError::new(
                Code::NotImplemented,
                "Refusing to apply without confirmation in a non-TTY environment.",
                "Re-run with --auto-approve.",
            ));
        }
        if !confirm(plan.creates.len(), plan.updates.len())? {
            return Ok(json!({ "cancelled": true }));
        }
    }
    let client = resolve_client(cli)?;
    let report = run_apply(&client, &state, &plan, ApplyOptions { dry_run })?;
    if !json_mode {
        if dry_run {
            println!("(dry-run — no changes made)");
        } else {
            println!(
                "\nApplied: {} created, {} updated",
                report.created, report.updated
            );
        }
    }
    Ok(json!({
        "created": report.created,
        "updated": report.updated,
        "dry_run": dry_run,
    }))
}

fn confirm(creates: usize, updates: usize) -> Result<bool, SyncError> {
    eprintln!(
        "About to apply: {} create, {} update. Continue? [y/N] ",
        creates, updates
    );
    io::stderr().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).map_err(|e| {
        SyncError::new(
            Code::NotImplemented,
            format!("Failed to read confirmation: {e}"),
            "Use --auto-approve to skip the prompt.",
        )
    })?;
    let answer = input.trim().to_lowercase();
    Ok(answer == "y" || answer == "yes")
}

// ─── import ───────────────────────────────────────────────────────────

fn cmd_import(
    cli: &SyncCli,
    provider: &str,
    kind: Option<&str>,
    json_mode: bool,
) -> Result<Value, SyncError> {
    let ws = load_workspace()?;
    let state = State::open(&ws.state_db_path())?;
    let client = resolve_client(cli)?;
    match provider {
        "klaviyo" => {
            let kind = kind.unwrap_or("templates");
            match kind {
                "templates" | "template" => {
                    let count = klaviyo::import_templates(&client, &ws, &state)?;
                    if !json_mode {
                        println!(
                            "Imported {} klaviyo template(s) into integrations/klaviyo/templates/.",
                            count
                        );
                        println!("Next: bisque-sync plan");
                    }
                    Ok(json!({
                        "provider": "klaviyo",
                        "kind": "template",
                        "imported": count,
                    }))
                }
                other => Err(SyncError::new(
                    Code::NotImplemented,
                    format!("import klaviyo {other} is not supported in the prototype."),
                    "Supported kinds: templates",
                )),
            }
        }
        other => Err(SyncError::new(
            Code::NotImplemented,
            format!("Provider '{other}' is not supported."),
            "Only `klaviyo` is wired up in the MVP.",
        )),
    }
}

// ─── render ───────────────────────────────────────────────────────────

fn cmd_render(resource: &str, json_mode: bool) -> Result<Value, SyncError> {
    let ws = load_workspace()?;
    let path = resolve_resource_path(&ws, resource)?;
    let raw = fs::read_to_string(&path).map_err(|e| {
        SyncError::new(
            Code::YamlParse,
            format!("Failed to read {}: {}", path.display(), e),
            "",
        )
    })?;
    let resource_val: klaviyo::TemplateResource = serde_yaml::from_str(&raw).map_err(|e| {
        SyncError::new(
            Code::YamlParse,
            format!("Failed to parse {}: {}", path.display(), e),
            "",
        )
    })?;
    let resource_val = resource_val.with_source(&path);
    let rendered = run_render(&resource_val.html, &ws.root, &resource_val.name_slug)?;
    if !json_mode {
        // Human mode: dump bytes to stdout so `> file.html` works. For a TTY,
        // bytes are almost certainly text so println! is fine.
        if io::stdout().is_terminal() {
            println!("{}", String::from_utf8_lossy(&rendered.bytes));
        } else {
            io::stdout().write_all(&rendered.bytes).ok();
        }
    }
    // In json mode we expose length + hash but not the bytes (stay machine-parseable).
    Ok(json!({
        "resource": resource_val.name_slug,
        "bytes": rendered.bytes.len(),
        "sha256": rendered.hash,
    }))
}

fn resolve_resource_path(ws: &Workspace, resource: &str) -> Result<std::path::PathBuf, SyncError> {
    // Candidate 1: a direct relative path provided.
    let direct = ws.root.join(resource);
    if direct.is_file() {
        return Ok(direct);
    }
    // Candidate 2: a slug inside integrations/klaviyo/templates.
    let candidates = [
        ws.root
            .join("integrations/klaviyo/templates")
            .join(format!("{resource}.yaml")),
        ws.root
            .join("integrations/klaviyo/templates")
            .join(format!("{}.yaml", resource.replace('_', "-"))),
    ];
    for c in candidates {
        if c.is_file() {
            return Ok(c);
        }
    }
    Err(SyncError::new(
        Code::NoWorkspace,
        format!("Could not find resource '{resource}'"),
        "Try a slug from `bisque-sync ls` or a path relative to the workspace root.",
    ))
}

// ─── doctor ───────────────────────────────────────────────────────────

fn cmd_doctor(cli: &SyncCli, json_mode: bool) -> Result<Value, SyncError> {
    let mut checks: Vec<Value> = Vec::new();

    // Workspace
    let ws_res = load_workspace();
    let ws = match &ws_res {
        Ok(ws) => {
            checks.push(check_pass("workspace", format!("bisque.yaml at {}", ws.root.display())));
            Some(ws.clone())
        }
        Err(e) => {
            checks.push(check_fail("workspace", format!("{}", e)));
            None
        }
    };

    // State DB
    if let Some(ws) = &ws {
        match State::open(&ws.state_db_path()) {
            Ok(state) => {
                let n = state.count_resources().unwrap_or(0);
                checks.push(check_pass("state_db", format!("{n} rows in resources")));
            }
            Err(e) => checks.push(check_fail("state_db", format!("{}", e))),
        }
    }

    // Auth
    match resolve_client(cli) {
        Ok(_) => checks.push(check_pass("auth", "bisque profile resolves".to_string())),
        Err(e) => checks.push(check_fail("auth", format!("{}", e))),
    }

    // Render dependencies: walk klaviyo templates, probe the first token of each command.
    if let Some(ws) = &ws {
        let providers = ws.providers().unwrap_or_default();
        for p in providers {
            if p.provider != "klaviyo" {
                continue;
            }
            let files = p.list_resource_files("template").unwrap_or_default();
            let mut missing = Vec::new();
            for path in &files {
                let raw = match fs::read_to_string(path) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let res: klaviyo::TemplateResource = match serde_yaml::from_str(&raw) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                if res.html.command.is_empty() {
                    missing.push(format!("{} (no command)", path.display()));
                } else if !command_on_path(&res.html.command[0]) {
                    missing.push(format!("{} (missing: {})", path.display(), res.html.command[0]));
                }
            }
            if missing.is_empty() {
                checks.push(check_pass(
                    "render_dependencies",
                    format!("{} klaviyo templates have runnable render commands", files.len()),
                ));
            } else {
                checks.push(check_warn(
                    "render_dependencies",
                    format!("{} template(s) have issues", missing.len()),
                ));
            }
        }
    }

    let any_fail = checks
        .iter()
        .any(|c| c.get("status").and_then(|v| v.as_str()) == Some("FAIL"));

    if !json_mode {
        for c in &checks {
            let status = c.get("status").and_then(|v| v.as_str()).unwrap_or("?");
            let name = c.get("check").and_then(|v| v.as_str()).unwrap_or("");
            let msg = c.get("message").and_then(|v| v.as_str()).unwrap_or("");
            println!("  [{status:<4}] {name}: {msg}");
        }
    }

    let report = json!({ "checks": checks, "ok": !any_fail });
    if any_fail {
        Err(SyncError::new(
            Code::NotImplemented,
            "One or more doctor checks failed.",
            "See the `checks` array for per-check remediation.",
        )
        .with_details(report))
    } else {
        Ok(report)
    }
}

fn check_pass(name: &str, message: String) -> Value {
    json!({ "check": name, "status": "PASS", "message": message })
}

fn check_warn(name: &str, message: String) -> Value {
    json!({ "check": name, "status": "WARN", "message": message })
}

fn check_fail(name: &str, message: String) -> Value {
    json!({ "check": name, "status": "FAIL", "message": message })
}

fn command_on_path(cmd: &str) -> bool {
    // Naive: ask `which` and accept exit 0.
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {cmd} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ─── help ─────────────────────────────────────────────────────────────

fn cmd_help(topic: &[String]) -> Result<Value, SyncError> {
    let body = crate::sync::help::render(topic)?;
    Ok(Value::String(body))
}

// ─── shared ───────────────────────────────────────────────────────────

fn resolve_client(cli: &SyncCli) -> Result<ApiClient, SyncError> {
    let config = config::load_config();
    let profile_name = config::resolve_profile_name(cli.profile.as_deref(), &config).map_err(|e| {
        SyncError::new(
            Code::AuthMissing,
            format!("{e}"),
            "Run `bisque login` or pass --profile.",
        )
    })?;
    let profile: Option<&BisqueProfile> = config::get_profile(&config, &profile_name);
    let auth = config::require_auth(
        cli.user_id.as_deref(),
        cli.api_key.as_deref(),
        cli.base_url.as_deref(),
        profile,
    )
    .map_err(|e| {
        SyncError::new(
            Code::AuthMissing,
            format!("{e}"),
            "Run `bisque login` to set up credentials.",
        )
    })?;
    Ok(ApiClient::new(auth.base_url, auth.user_id, auth.api_key))
}
