use crate::api::ApiClient;
use crate::config::{self, BisqueConfig, BisqueProfile};
use crate::{
    Cli, Command, CORE_SKILL_DIR_NAME, DISCOVERY_SKILL_DIR_NAME,
    GENERATED_SKILL_PREFIX,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

// ─── Embedded SKILL.md template ─────────────────────────────────────

const CORE_SKILL_MD: &str = include_str!("../templates/core-skill.md");

// ─── Types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapTool {
    name: String,
    description: String,
    #[serde(default)]
    parameters: Value,
    // v0 fields (kept for backwards compat)
    integration_id: Option<String>,
    web_integration_id: Option<String>,
    access: Option<String>,
    safe: Option<bool>,
    // v1 fields
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    toolbox_id: Option<String>,
    #[serde(default)]
    provider_id: Option<String>,
}

struct IntegrationGroup {
    integration_id: String,
    #[allow(dead_code)]
    web_integration_id: String,
    label: String,
    tools: Vec<BootstrapTool>,
}

// ─── Command dispatch ───────────────────────────────────────────────

pub fn run(cli: Cli) -> Result<()> {
    let config = config::load_config();
    let profile_name =
        config::resolve_profile_name(cli.profile.as_deref(), &config);

    // Validate profile exists (skip for login which creates profiles)
    if !matches!(cli.command, Command::Login) {
        if cli.profile.is_some()
            && config::get_profile(&config, &profile_name).is_none()
        {
            bail!(
                "Profile \"{}\" not found in {}",
                profile_name,
                config::config_path().display()
            );
        }
    }

    let profile = config::get_profile(&config, &profile_name).cloned();

    match &cli.command {
        Command::Login => {
            cmd_login(&cli, &config, &profile_name, profile.as_ref())
        }
        Command::Init {
            agent,
            skills_dir,
            force,
        } => cmd_init(
            &cli,
            profile.as_ref(),
            agent.as_deref(),
            skills_dir.as_deref(),
            *force,
        ),
        Command::Doctor => cmd_doctor(&cli, profile.as_ref()),
        Command::Tools => cmd_tools(&cli, profile.as_ref()),
        Command::Bootstrap => cmd_bootstrap(&cli, profile.as_ref()),
        Command::Call {
            tool_name,
            args_json,
            invocation_id,
        } => cmd_call(
            &cli,
            profile.as_ref(),
            tool_name,
            args_json.as_deref(),
            invocation_id.as_deref(),
        ),
        Command::Sync { agent, skills_dir } => cmd_sync(
            &cli,
            profile.as_ref(),
            agent.as_deref(),
            skills_dir.as_deref(),
        ),
        Command::Connect { integration } => {
            cmd_connect(&cli, profile.as_ref(), integration)
        }
        Command::Update => cmd_update(&cli, profile.as_ref()),
    }
}

// ─── Login ──────────────────────────────────────────────────────────

fn cmd_login(
    cli: &Cli,
    config: &Option<BisqueConfig>,
    profile_name: &str,
    existing_profile: Option<&BisqueProfile>,
) -> Result<()> {
    let has_manual_flags =
        cli.user_id.is_some() || cli.api_key.is_some();

    // If interactive (no flags) and terminal, use browser flow
    if !has_manual_flags && io::stdin().is_terminal() {
        let env_base_url = std::env::var("BISQUE_BASE_URL").ok();
        let base_url = cli
            .base_url
            .as_deref()
            .or(env_base_url.as_deref())
            .or(existing_profile.and_then(|p| p.base_url.as_deref()))
            .unwrap_or(crate::DEFAULT_BASE_URL)
            .trim_end_matches('/');

        return browser_login(
            base_url,
            config,
            profile_name,
            existing_profile,
        );
    }

    // Manual fallback: --user-id / --api-key flags
    manual_login(cli, config, profile_name, existing_profile)
}

fn browser_login(
    base_url: &str,
    config: &Option<BisqueConfig>,
    profile_name: &str,
    existing_profile: Option<&BisqueProfile>,
) -> Result<()> {
    // 1. Create session
    let url = format!("{base_url}/api/create-cli-session");
    let resp = ureq::post(&url)
        .set("Content-Type", "application/json")
        .send_string("{}")
        .context("Failed to create CLI session")?;

    let body: Value = serde_json::from_str(
        &resp.into_string().context("Failed to read response")?,
    )?;

    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .context("Missing token in response")?;
    let pairing_code = body
        .get("pairingCode")
        .and_then(|v| v.as_str())
        .context("Missing pairingCode in response")?;
    let browser_url = body
        .get("browserUrl")
        .and_then(|v| v.as_str())
        .context("Missing browserUrl in response")?;

    // 2. Open browser and show pairing code
    let _ = open::that(browser_url);

    eprintln!();
    eprintln!("  Pairing code:  {pairing_code}");
    eprintln!();
    eprintln!("  If the browser didn't open, visit:");
    eprintln!("  {browser_url}");
    eprintln!();
    eprintln!("  Waiting for confirmation... (^C to quit)");
    let poll_url =
        format!("{base_url}/api/poll-cli-session?t={token}");
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(300);

    loop {
        if std::time::Instant::now() > deadline {
            bail!(
                "Session expired. Run `bisque login` to try again."
            );
        }

        std::thread::sleep(std::time::Duration::from_secs(1));

        let poll_resp = match ureq::get(&poll_url).call() {
            Ok(r) => r,
            Err(_) => continue, // transient network error, retry
        };

        let poll_body: Value = match poll_resp.into_string() {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => continue,
        };

        let status = poll_body
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("pending");

        match status {
            "approved" => {
                let user_id = poll_body
                    .get("userId")
                    .and_then(|v| v.as_str())
                    .context("Missing userId in approval")?
                    .to_string();
                let api_key = poll_body
                    .get("apiKey")
                    .and_then(|v| v.as_str())
                    .context("Missing apiKey in approval")?
                    .to_string();

                // Save credentials
                let mut cfg =
                    config.clone().unwrap_or_default();
                let profiles =
                    cfg.profiles.get_or_insert_with(HashMap::new);
                let profile = BisqueProfile {
                    user_id: Some(user_id.clone()),
                    api_key: Some(api_key.clone()),
                    ..existing_profile.cloned().unwrap_or_default()
                };
                profiles
                    .insert(profile_name.to_string(), profile);

                if cfg.active_profile.is_none() {
                    cfg.active_profile =
                        Some("default".to_string());
                }

                config::save_config(&cfg)?;
                let path = config::config_path();
                eprintln!(
                    "\nDone! Credentials saved to {}",
                    path.display()
                );
                return Ok(());
            }
            "expired" => {
                bail!(
                    "Session expired. Run `bisque login` to try again."
                );
            }
            _ => {
                // still pending, keep polling
            }
        }
    }
}

fn manual_login(
    cli: &Cli,
    config: &Option<BisqueConfig>,
    profile_name: &str,
    existing_profile: Option<&BisqueProfile>,
) -> Result<()> {
    let mut config = config.clone().unwrap_or_default();
    let profiles = config.profiles.get_or_insert_with(HashMap::new);

    // Show existing profile if present and no flags given
    if let Some(existing) = existing_profile {
        if cli.user_id.is_none() && cli.api_key.is_none() {
            eprintln!("Current profile \"{profile_name}\":");
            eprintln!(
                "  userId: {}",
                mask_str(existing.user_id.as_deref(), 8)
            );
            eprintln!(
                "  apiKey: {}",
                mask_str(existing.api_key.as_deref(), 12)
            );
            eprintln!(
                "  baseUrl: {}",
                existing
                    .base_url
                    .as_deref()
                    .unwrap_or(crate::DEFAULT_BASE_URL)
            );
            eprintln!();
        }
    }

    let mut user_id = cli.user_id.clone().unwrap_or_default();
    let mut api_key = cli.api_key.clone().unwrap_or_default();

    // Interactive prompts if not provided via flags
    if user_id.is_empty() || api_key.is_empty() {
        if !io::stdin().is_terminal() {
            bail!(
                "Non-interactive mode requires --user-id and --api-key flags."
            );
        }
        eprintln!("Enter credentials (found at bisque.tools/settings):\n");
        if user_id.is_empty() {
            user_id = prompt("  User ID: ")?;
        }
        if api_key.is_empty() {
            api_key = prompt("  API key: ")?;
        }
    }

    if user_id.is_empty() || api_key.is_empty() {
        bail!("Both user ID and API key are required.");
    }

    // Update profile
    let profile = BisqueProfile {
        user_id: Some(user_id.clone()),
        api_key: Some(api_key.clone()),
        ..existing_profile.cloned().unwrap_or_default()
    };
    profiles.insert(profile_name.to_string(), profile);

    if config.active_profile.is_none() {
        config.active_profile = Some("default".to_string());
    }

    config::save_config(&config)?;
    let path = config::config_path();
    eprint!("\nCredentials saved to {}", path.display());
    if profile_name != "default" {
        eprintln!(" (profile: {profile_name})");
    } else {
        eprintln!();
    }

    // Verify auth
    eprintln!("\nVerifying credentials...");
    let env_base_url = std::env::var("BISQUE_BASE_URL").ok();
    let base_url = cli
        .base_url
        .as_deref()
        .or(env_base_url.as_deref())
        .or(existing_profile.and_then(|p| p.base_url.as_deref()))
        .unwrap_or(crate::DEFAULT_BASE_URL)
        .trim_end_matches('/');

    let client = ApiClient::new(
        base_url.to_string(),
        user_id,
        api_key,
    );

    match client.get_json("/v1/bootstrap") {
        Ok(result) => {
            let count = result
                .get("tools")
                .and_then(|t| t.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            eprintln!("  Auth OK — {count} tool(s) available.");
        }
        Err(e) => {
            eprintln!("  Auth failed: {e}");
            eprintln!(
                "  Credentials were saved. Check your user ID and API key."
            );
            std::process::exit(1);
        }
    }

    Ok(())
}

// ─── Init ───────────────────────────────────────────────────────────

fn cmd_init(
    cli: &Cli,
    profile: Option<&BisqueProfile>,
    agent: Option<&str>,
    skills_dir: Option<&str>,
    force: bool,
) -> Result<()> {
    // 1. Check credentials
    let auth = config::resolve_auth(
        cli.user_id.as_deref(),
        cli.api_key.as_deref(),
        cli.base_url.as_deref(),
        profile,
    );
    if auth.user_id.is_empty() || auth.api_key.is_empty() {
        bail!("No credentials found. Run `bisque login` first.");
    }

    // 2. Resolve agent / skills root
    let effective_agent: Option<&str> = if skills_dir.is_some() {
        None
    } else {
        agent.or_else(|| {
            let detected = detect_agent();
            if let Some(a) = detected {
                eprintln!("Detected agent: {a}");
            }
            detected
        })
    };

    if skills_dir.is_none() && effective_agent.is_none() {
        bail!("Could not auto-detect agent. Use --agent or --skills-dir.");
    }

    let skills_root = resolve_skills_root(skills_dir, effective_agent)?;
    let bisque_api_dir = skills_root.join(CORE_SKILL_DIR_NAME);

    eprintln!("Skills root: {}", skills_root.display());
    eprintln!("Bisque skill: {}\n", bisque_api_dir.display());

    // 3. Check existing
    let skill_md_path = bisque_api_dir.join("SKILL.md");
    if skill_md_path.exists() && !force {
        bail!(
            "SKILL.md already exists at {}\nUse --force to overwrite.",
            skill_md_path.display()
        );
    }

    // 4. Write SKILL.md
    fs::create_dir_all(&bisque_api_dir)
        .context("Failed to create skill directory")?;
    fs::write(&skill_md_path, CORE_SKILL_MD)
        .context("Failed to write SKILL.md")?;
    eprintln!("Wrote {}", skill_md_path.display());

    // 5. Run sync
    eprintln!("\nSyncing integrations...");
    let client =
        ApiClient::new(auth.base_url, auth.user_id, auth.api_key);
    run_sync_inner(&client, &skills_root)?;

    // 6. Summary
    eprintln!("\nDone. Bisque is ready to use.");
    eprintln!("  bisque tools     — list available tools");
    eprintln!("  bisque call <t>  — execute a tool");
    eprintln!("  bisque doctor    — check setup health");
    Ok(())
}

// ─── Toolbox discovery ──────────────────────────────────────────────

/// Fetch connected toolbox IDs from /v1/toolboxes and return a bootstrap
/// URL that pre-loads all of them.
fn full_bootstrap_path(client: &ApiClient) -> String {
    let toolbox_ids = match client.get_json("/v1/toolboxes") {
        Ok(result) => {
            let mut ids = Vec::new();
            if let Some(providers) = result.get("providers").and_then(|v| v.as_array()) {
                for provider in providers {
                    let connected = provider
                        .get("connected")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if !connected {
                        continue;
                    }
                    if let Some(toolboxes) = provider.get("toolboxes").and_then(|v| v.as_array()) {
                        for tb in toolboxes {
                            if let Some(id) = tb.get("id").and_then(|v| v.as_str()) {
                                ids.push(id.to_string());
                            }
                        }
                    }
                }
            }
            ids
        }
        Err(_) => Vec::new(),
    };

    if toolbox_ids.is_empty() {
        "/v1/bootstrap".to_string()
    } else {
        format!("/v1/bootstrap?toolboxes={}", toolbox_ids.join(","))
    }
}

// ─── Tools ──────────────────────────────────────────────────────────

fn cmd_tools(cli: &Cli, profile: Option<&BisqueProfile>) -> Result<()> {
    let auth = config::require_auth(
        cli.user_id.as_deref(),
        cli.api_key.as_deref(),
        cli.base_url.as_deref(),
        profile,
    )?;
    let client =
        ApiClient::new(auth.base_url, auth.user_id, auth.api_key);
    let path = full_bootstrap_path(&client);
    let result = client.get_json(&path)?;
    let tools = parse_tools(&result);

    if cli.json {
        let arr: Vec<Value> = tools
            .iter()
            .map(|t| serde_json::to_value(t).unwrap())
            .collect();
        print_json(&Value::Array(arr), cli.pretty);
    } else {
        for tool in &tools {
            println!("{} — {}", tool.name, tool.description);
        }
        if tools.is_empty() {
            eprintln!("No tools available.");
        }
    }
    Ok(())
}

// ─── Bootstrap ──────────────────────────────────────────────────────

fn cmd_bootstrap(
    cli: &Cli,
    profile: Option<&BisqueProfile>,
) -> Result<()> {
    let auth = config::require_auth(
        cli.user_id.as_deref(),
        cli.api_key.as_deref(),
        cli.base_url.as_deref(),
        profile,
    )?;
    let client =
        ApiClient::new(auth.base_url, auth.user_id, auth.api_key);
    let result = client.get_json("/v1/bootstrap")?;
    print_result(&result, cli);
    Ok(())
}

// ─── Call ────────────────────────────────────────────────────────────

fn cmd_call(
    cli: &Cli,
    profile: Option<&BisqueProfile>,
    tool_name: &str,
    args_json: Option<&str>,
    invocation_id: Option<&str>,
) -> Result<()> {
    let auth = config::require_auth(
        cli.user_id.as_deref(),
        cli.api_key.as_deref(),
        cli.base_url.as_deref(),
        profile,
    )?;
    let client =
        ApiClient::new(auth.base_url, auth.user_id, auth.api_key);

    let raw_args = args_json
        .map(String::from)
        .unwrap_or_else(read_stdin_if_piped);

    let args: Value = if raw_args.is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        let parsed: Value =
            serde_json::from_str(&raw_args).context("Invalid JSON for args")?;
        if !parsed.is_object() {
            bail!("Tool args must be a JSON object.");
        }
        parsed
    };

    let mut body = serde_json::json!({
        "toolName": tool_name,
        "args": args,
    });
    if let Some(id) = invocation_id {
        body["invocationId"] = Value::String(id.to_string());
    }

    let result = client.post_json("/v1/tool-call", &body)?;
    print_result(&result, cli);
    Ok(())
}

// ─── Sync ───────────────────────────────────────────────────────────

fn cmd_sync(
    cli: &Cli,
    profile: Option<&BisqueProfile>,
    agent: Option<&str>,
    skills_dir: Option<&str>,
) -> Result<()> {
    let auth = config::require_auth(
        cli.user_id.as_deref(),
        cli.api_key.as_deref(),
        cli.base_url.as_deref(),
        profile,
    )?;
    let client =
        ApiClient::new(auth.base_url, auth.user_id, auth.api_key);

    let detected;
    let effective_agent: Option<&str> = if skills_dir.is_some() {
        None
    } else if agent.is_some() {
        agent
    } else {
        detected = detect_agent();
        detected
    };

    let skills_root = resolve_skills_root(skills_dir, effective_agent)?;
    run_sync_inner(&client, &skills_root)
}

fn run_sync_inner(client: &ApiClient, skills_root: &Path) -> Result<()> {
    let bisque_api_dir = skills_root.join(CORE_SKILL_DIR_NAME);
    eprintln!("Skills root: {}", skills_root.display());
    eprintln!("Bisque API dir: {}\n", bisque_api_dir.display());

    let path = full_bootstrap_path(client);
    let result = client.get_json(&path)?;
    let tools = parse_tools(&result);

    if tools.is_empty() {
        eprintln!("No tools returned by bootstrap. Nothing to sync.");
        return Ok(());
    }

    let groups = group_tools_by_integration(&tools);
    let existing_dirs = find_existing_generated_dirs(skills_root);
    let mut current_dirs = HashSet::new();

    let mut added = Vec::new();
    let mut updated = Vec::new();

    for group in &groups {
        let dir_name = skill_dir_name(&group.integration_id);
        current_dirs.insert(dir_name.clone());
        let dir_path = skills_root.join(&dir_name);
        let existed = existing_dirs.contains(&dir_name);

        fs::create_dir_all(&dir_path)?;
        fs::write(
            dir_path.join("SKILL.md"),
            generate_integration_skill_md(group),
        )?;
        fs::write(
            dir_path.join("tools.json"),
            generate_tools_json(group),
        )?;

        if existed {
            updated.push(dir_name);
        } else {
            added.push(dir_name);
        }
    }

    // Remove stale integration dirs (but not the discovery skill)
    let mut removed = Vec::new();
    for dir in &existing_dirs {
        if !current_dirs.contains(dir)
            && dir != DISCOVERY_SKILL_DIR_NAME
        {
            let _ = fs::remove_dir_all(skills_root.join(dir));
            removed.push(dir.clone());
        }
    }

    // Generate or remove the discovery skill for unconnected integrations
    let has_discovery = sync_discovery_skill(&result, skills_root)?;

    // Refresh core SKILL.md from template (keeps it in sync with CLI updates)
    if bisque_api_dir.exists() {
        fs::write(bisque_api_dir.join("SKILL.md"), CORE_SKILL_MD)?;
    }

    // Summary
    eprintln!("Synced {} integration(s):", groups.len());
    for name in &added {
        eprintln!("  + {name} (new)");
    }
    for name in &updated {
        eprintln!("  ~ {name} (updated)");
    }
    for name in &removed {
        eprintln!("  - {name} (removed)");
    }
    if has_discovery {
        eprintln!("  ~ {DISCOVERY_SKILL_DIR_NAME} (discovery)");
    }
    if added.is_empty() && updated.is_empty() && removed.is_empty() {
        eprintln!("  (no changes)");
    }

    Ok(())
}

/// Generates or removes the `bisque-available-integrations` discovery skill.
///
/// Lists all unconnected integrations with `bisque connect` commands so the
/// agent can suggest new integrations when relevant. If every integration
/// is connected, the discovery skill directory is removed.
fn sync_discovery_skill(
    bootstrap: &Value,
    skills_root: &Path,
) -> Result<bool> {
    let dir_path = skills_root.join(DISCOVERY_SKILL_DIR_NAME);

    let status = match bootstrap
        .get("integrationStatus")
        .and_then(|v| v.as_object())
    {
        Some(s) => s,
        None => {
            let _ = fs::remove_dir_all(&dir_path);
            return Ok(false);
        }
    };

    // Integrations that are not agent-connectable (skip them)
    let skip = ["google", "imessage", "weather"];

    let mut unconnected: Vec<(String, String)> = Vec::new();
    for (key, value) in status {
        if skip.contains(&key.as_str()) {
            continue;
        }
        let connected = value
            .get("connected")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !connected {
            let label = integration_label(key).to_string();
            let slug = integration_slug(key);
            unconnected.push((label, slug));
        }
    }

    if unconnected.is_empty() {
        let _ = fs::remove_dir_all(&dir_path);
        return Ok(false);
    }

    unconnected.sort_by(|a, b| a.0.cmp(&b.0));

    // Build a short list of names for the frontmatter description
    let names: Vec<&str> =
        unconnected.iter().map(|(l, _)| l.as_str()).collect();
    let names_summary = if names.len() <= 4 {
        names.join(", ")
    } else {
        format!(
            "{}, and {} more",
            names[..3].join(", "),
            names.len() - 3
        )
    };

    let mut rows = String::new();
    for (label, slug) in &unconnected {
        rows.push_str(&format!(
            "| {label} | `bisque connect {slug}` |\n"
        ));
    }

    let content = format!(
        r#"---
name: Available Integrations
description: Discover and connect new API integrations via Bisque — {names_summary}.
---

# Available Integrations

These integrations are available but not yet connected. If any would help
with the current task, suggest it to the user and run the connect command.

| Integration | Connect command |
|-------------|----------------|
{rows}
Running `bisque connect <name>` opens the browser to the setup page.
After the user completes setup, run `bisque sync` to refresh available tools.
"#,
        names_summary = names_summary,
        rows = rows,
    );

    fs::create_dir_all(&dir_path)?;
    fs::write(dir_path.join("SKILL.md"), content)?;
    Ok(true)
}

// ─── Doctor ─────────────────────────────────────────────────────────

fn cmd_doctor(
    cli: &Cli,
    profile: Option<&BisqueProfile>,
) -> Result<()> {
    let mut ok = true;

    // 1. Config file
    println!("Checking credentials...");
    let config_path = config::config_path();
    if config_path.exists() {
        println!("  config found at {}", config_path.display());
    } else {
        println!("  config NOT found at {}", config_path.display());
        ok = false;
    }

    let auth = config::resolve_auth(
        cli.user_id.as_deref(),
        cli.api_key.as_deref(),
        cli.base_url.as_deref(),
        profile,
    );

    if auth.user_id.is_empty() {
        println!("  BISQUE_USER_ID: missing");
        ok = false;
    } else {
        println!("  BISQUE_USER_ID: {}", mask_str(Some(&auth.user_id), 8));
    }

    if auth.api_key.is_empty() {
        println!("  BISQUE_API_KEY: missing");
        ok = false;
    } else {
        println!(
            "  BISQUE_API_KEY: {}",
            mask_str(Some(&auth.api_key), 12)
        );
    }

    if auth.user_id.is_empty() || auth.api_key.is_empty() {
        println!("\nCannot check API connectivity without credentials.");
        std::process::exit(if ok { 0 } else { 1 });
    }

    // 2. Bootstrap auth
    println!("\nChecking API ({})...", auth.base_url);
    let client = ApiClient::new(
        auth.base_url,
        auth.user_id,
        auth.api_key,
    );

    let bootstrap_path = full_bootstrap_path(&client);
    let result = match client.get_json(&bootstrap_path) {
        Ok(r) => {
            println!("  Auth: OK");
            r
        }
        Err(e) => {
            println!("  Auth: FAILED — {e}");
            std::process::exit(1);
        }
    };

    // 3. Integration status
    let tools = parse_tools(&result);
    println!("\nIntegrations:");

    if let Some(status) = result
        .get("integrationStatus")
        .and_then(|v| v.as_object())
    {
        let mut connected = Vec::new();
        let mut disconnected = Vec::new();

        for (key, value) in status {
            if value
                .get("connected")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                connected.push(key.as_str());
            } else {
                disconnected.push(key.as_str());
            }
        }

        connected.sort();
        disconnected.sort();

        if !connected.is_empty() {
            println!("  Connected:");
            for name in &connected {
                println!("    + {name}");
            }
        }
        if !disconnected.is_empty() {
            println!("  Available (not connected):");
            for name in &disconnected {
                let slug = integration_slug(name);
                println!(
                    "    - {} (bisque connect {slug})",
                    integration_label(name)
                );
            }
        }
    }

    // 4. Stale generated dirs
    if let Ok(skills_root) = resolve_skills_root(None, None) {
        let existing_dirs = find_existing_generated_dirs(&skills_root);
        let groups = group_tools_by_integration(&tools);
        let current_dirs: HashSet<String> = groups
            .iter()
            .map(|g| skill_dir_name(&g.integration_id))
            .collect();

        let stale: Vec<&String> = existing_dirs
            .iter()
            .filter(|d| !current_dirs.contains(*d))
            .collect();

        if !stale.is_empty() {
            println!("\n  Stale skill directories (run sync to clean up):");
            for name in &stale {
                println!("    ! {name}");
            }
            ok = false;
        }
    }

    let group_count = group_tools_by_integration(&tools).len();
    println!(
        "\nTools: {} available across {} integration(s)",
        tools.len(),
        group_count
    );
    println!(
        "{}",
        if ok {
            "\nAll checks passed."
        } else {
            "\nSome issues found."
        }
    );
    std::process::exit(if ok { 0 } else { 1 });
}

// ─── Connect ────────────────────────────────────────────────────────

/// Maps integration status keys to web UI URL paths.
fn integration_url_path(integration: &str) -> Option<&'static str> {
    match integration {
        "google" | "gmail" | "google-calendar" | "googleCalendar"
        | "google-analytics" | "googleAnalytics"
        | "google-search-console" | "googleSearchConsole"
        | "google-tag-manager" | "googleTagManager"
        | "google-sheets" | "googleSheets" => {
            Some("/integrations/google")
        }
        "tesla" => Some("/integrations/tesla"),
        "stripe" => Some("/integrations/stripe"),
        "mailchimp" => Some("/integrations/mailchimp"),
        "klaviyo" => Some("/integrations/klaviyo"),
        "x" => Some("/integrations/x"),
        "meta-ads" | "metaAds" => Some("/integrations/meta-ads"),
        "reddit-ads" | "redditAds" => {
            Some("/integrations/reddit-ads")
        }
        _ => None,
    }
}

/// Human-readable label for an integration status key.
fn integration_label(key: &str) -> &str {
    match key {
        "google" => "Google",
        "gmail" => "Gmail",
        "googleCalendar" => "Google Calendar",
        "tesla" => "Tesla",
        "imessage" => "iMessage",
        "weather" => "Weather",
        "stripe" => "Stripe",
        "mailchimp" => "Mailchimp",
        "klaviyo" => "Klaviyo",
        "x" => "X (Twitter)",
        "ahrefs" => "Ahrefs",
        "metaAds" => "Meta Ads",
        "redditAds" => "Reddit Ads",
        "googleAnalytics" => "Google Analytics",
        "googleSearchConsole" => "Google Search Console",
        "googleTagManager" => "Google Tag Manager",
        "googleSheets" => "Google Sheets",
        "firestore" => "Firestore",
        _ => key,
    }
}

/// Slug form for CLI usage (e.g., "googleAnalytics" → "google-analytics").
fn integration_slug(key: &str) -> String {
    // Convert camelCase to kebab-case
    let mut result = String::new();
    for (i, ch) in key.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            result.push('-');
            result.push(ch.to_lowercase().next().unwrap());
        } else {
            result.push(ch);
        }
    }
    result
}

fn cmd_connect(
    cli: &Cli,
    profile: Option<&BisqueProfile>,
    integration: &str,
) -> Result<()> {
    let env_base_url = std::env::var("BISQUE_BASE_URL").ok();
    let base_url = cli
        .base_url
        .as_deref()
        .or(env_base_url.as_deref())
        .or(profile.and_then(|p| p.base_url.as_deref()))
        .unwrap_or(crate::DEFAULT_BASE_URL)
        .trim_end_matches('/');

    let path = match integration_url_path(integration) {
        Some(p) => p,
        None => {
            eprintln!(
                "Unknown integration: \"{integration}\"\n"
            );
            eprintln!("Available integrations:");
            eprintln!("  google, klaviyo, mailchimp, meta-ads,");
            eprintln!("  reddit-ads, stripe, tesla, x");
            eprintln!(
                "\nGoogle sub-services (google-analytics, google-calendar, etc.)"
            );
            eprintln!(
                "all connect through the Google integration page."
            );
            std::process::exit(1);
        }
    };

    let url = format!("{base_url}{path}");
    eprintln!("Opening {url}");
    open::that(&url).context("Failed to open browser")?;
    Ok(())
}

// ─── Sync helpers ───────────────────────────────────────────────────

fn detect_agent() -> Option<&'static str> {
    let home = dirs::home_dir()?;
    if home.join(".claude").exists() {
        return Some("claude-code");
    }
    if home.join(".codex").exists() {
        return Some("codex");
    }
    None
}

fn resolve_skills_root(
    skills_dir: Option<&str>,
    agent: Option<&str>,
) -> Result<PathBuf> {
    if let Some(dir) = skills_dir {
        return Ok(PathBuf::from(dir));
    }

    if let Some(agent) = agent {
        let home =
            dirs::home_dir().context("Cannot determine home directory")?;
        return match agent {
            "claude-code" => Ok(home.join(".claude").join("skills")),
            "codex" => {
                let codex_home = std::env::var("CODEX_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| home.join(".codex"));
                Ok(codex_home.join("skills"))
            }
            _ => {
                bail!(
                    "Unknown agent: {agent}. Use \"claude-code\" or \"codex\"."
                )
            }
        };
    }

    // Fallback: CLAUDE_SKILL_DIR env → auto-detect
    if let Ok(skill_dir) = std::env::var("CLAUDE_SKILL_DIR") {
        let path = PathBuf::from(&skill_dir);
        if let Some(parent) = path.parent() {
            return Ok(parent.to_path_buf());
        }
    }

    let home =
        dirs::home_dir().context("Cannot determine home directory")?;
    if home.join(".claude").exists() {
        return Ok(home.join(".claude").join("skills"));
    }
    if home.join(".codex").exists() {
        return Ok(home.join(".codex").join("skills"));
    }

    bail!("Could not determine skills directory. Use --skills-dir or --agent.");
}

fn parse_tools(result: &Value) -> Vec<BootstrapTool> {
    result
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    serde_json::from_value::<BootstrapTool>(v.clone()).ok()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn group_tools_by_integration(
    tools: &[BootstrapTool],
) -> Vec<IntegrationGroup> {
    let mut map: HashMap<String, IntegrationGroup> = HashMap::new();

    for tool in tools {
        // v1 uses provider_id; v0 uses integration_id
        let id_opt = tool
            .integration_id
            .as_ref()
            .or(tool.provider_id.as_ref());
        if let Some(id) = id_opt {
            let group = map.entry(id.clone()).or_insert_with(|| {
                IntegrationGroup {
                    integration_id: id.clone(),
                    web_integration_id: tool
                        .web_integration_id
                        .clone()
                        .unwrap_or_else(|| id.clone()),
                    label: label_from_integration_id(id),
                    tools: Vec::new(),
                }
            });
            group.tools.push(tool.clone());
        }
    }

    let mut groups: Vec<IntegrationGroup> = map.into_values().collect();
    groups.sort_by(|a, b| a.integration_id.cmp(&b.integration_id));
    groups
}

fn label_from_integration_id(id: &str) -> String {
    id.split('-')
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => {
                    c.to_uppercase().to_string() + chars.as_str()
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn skill_dir_name(integration_id: &str) -> String {
    format!("{GENERATED_SKILL_PREFIX}{integration_id}")
}

fn find_existing_generated_dirs(skills_root: &Path) -> HashSet<String> {
    let mut dirs = HashSet::new();
    if let Ok(entries) = fs::read_dir(skills_root) {
        for entry in entries.flatten() {
            if let Ok(ft) = entry.file_type() {
                if ft.is_dir() {
                    let name =
                        entry.file_name().to_string_lossy().to_string();
                    if name.starts_with(GENERATED_SKILL_PREFIX)
                        && name != CORE_SKILL_DIR_NAME
                        && name != DISCOVERY_SKILL_DIR_NAME
                    {
                        dirs.insert(name);
                    }
                }
            }
        }
    }
    dirs
}

/// Return the `expand_scopes` section for Google integrations that have write
/// tools, or an empty string for everything else.
fn google_scope_expansion_section(group: &IntegrationGroup) -> String {
    // Always include scope expansion for known Google integrations.
    // Write tools may be filtered out by the backend when the user lacks
    // write scopes, but the section is still needed so the agent knows how
    // to guide the user through granting those scopes.
    let scope_info: Option<(&str, &str)> = match group.integration_id.as_str() {
        "google-tag-manager" => Some((
            "googleTagManager",
            "tagmanager.edit_containers,tagmanager.publish",
        )),
        "google-gmail" => Some(("gmail", "gmail.modify")),
        "google-calendar" => Some(("googleCalendar", "calendar.events")),
        "google-sheets" => Some(("googleSheets", "sheets")),
        "google-search-console" => {
            Some(("googleSearchConsole", "searchconsole"))
        }
        _ => None,
    };

    let (web_id, scopes) = match scope_info {
        Some(pair) => pair,
        None => return String::new(),
    };

    format!(
        r#"
## Scope expansion

This integration supports write operations that may require additional OAuth
scopes. If a tool call fails with a **403 "insufficient authentication scopes"**
error, or if write tools are not listed above, the user needs to grant
additional scopes. Run:

```bash
open "https://bisque.tools/integrations?integration={web_id}&expand_scopes={scopes}"
```

This auto-triggers the Google OAuth consent screen for the missing scopes.
Once the user approves, re-run `bisque sync` to refresh available tools, then
retry the tool call.
"#,
        web_id = web_id,
        scopes = scopes,
    )
}

fn generate_integration_skill_md(group: &IntegrationGroup) -> String {
    let tool_rows: String = group
        .tools
        .iter()
        .map(|t| {
            format!(
                "| {} | {} | {} |",
                t.name,
                t.description,
                t.access.as_deref().unwrap_or("read")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let plural = if group.tools.len() == 1 { "" } else { "s" };
    let scope_section = google_scope_expansion_section(group);

    format!(
        r#"---
name: {label}
description: {label} integration via Bisque — {count} tool{plural} available.
---

# {label}

You have access to the user's {label} integration via Bisque.

## Available tools

| Tool | Description | Access |
|------|-------------|--------|
{tool_rows}

## Usage

Call any tool above using:

```bash
bisque call <toolName> --args '<json>'
```

## Parse the result

Tool call responses are JSON with this shape:

```json
{{
  "status": "succeeded",
  "data": {{}}
}}
```

- `status` is `"succeeded"`, `"failed"`, or `"denied"`.
- `data` contains the raw API response.
- If `status` is `"failed"`, an `error` string is included.
- Use `--field data` to extract just the API response data.
{scope_section}
## Guidelines

- Summarize the `data` field for the user — do not dump raw JSON
  unless they ask for details.
- If a tool returns `"denied"`, tell the user the integration may need
  re-authorization at bisque.tools.
- If a tool returns a 403 scope error, use `open` to launch the scope
  expansion URL as described above, then retry after the user approves.
- Use `--pretty` if you need human-readable JSON output.
"#,
        label = group.label,
        count = group.tools.len(),
        plural = plural,
        tool_rows = tool_rows,
        scope_section = scope_section,
    )
}

fn generate_tools_json(group: &IntegrationGroup) -> String {
    let tools: Vec<Value> = group
        .tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
                "access": t.access.as_deref().unwrap_or("read"),
                "safe": t.safe.unwrap_or(false),
            })
        })
        .collect();
    serde_json::to_string_pretty(&tools).unwrap() + "\n"
}

// ─── Output helpers ─────────────────────────────────────────────────

fn print_json(value: &Value, pretty: bool) {
    let output = if pretty {
        serde_json::to_string_pretty(value).unwrap()
    } else {
        serde_json::to_string(value).unwrap()
    };
    println!("{output}");
}

fn print_result(result: &Value, cli: &Cli) {
    if cli.summary_only {
        // v1 tool-call: check error, then status
        let summary = result
            .get("error")
            .and_then(|v| v.as_str())
            .or_else(|| result.get("summary").and_then(|v| v.as_str()))
            .or_else(|| {
                result
                    .get("result")
                    .and_then(|r| r.get("summary"))
                    .and_then(|v| v.as_str())
            })
            .or_else(|| result.get("status").and_then(|v| v.as_str()));
        match summary {
            Some(s) => println!("{s}"),
            None => print_json(result, cli.pretty),
        }
        return;
    }

    if let Some(ref field_path) = cli.field {
        match get_nested_field(result, field_path) {
            Some(value) => {
                if let Some(s) = value.as_str() {
                    println!("{s}");
                } else {
                    print_json(value, cli.pretty);
                }
            }
            None => {
                eprintln!("Field \"{field_path}\" not found.");
                std::process::exit(1);
            }
        }
        return;
    }

    print_json(result, cli.pretty);
}

fn get_nested_field<'a>(
    value: &'a Value,
    path: &str,
) -> Option<&'a Value> {
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

// ─── IO helpers ─────────────────────────────────────────────────────

fn prompt(message: &str) -> Result<String> {
    eprint!("{message}");
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn read_stdin_if_piped() -> String {
    if io::stdin().is_terminal() {
        return String::new();
    }
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf).unwrap_or_default();
    buf.trim().to_string()
}

fn mask_str(value: Option<&str>, visible: usize) -> String {
    match value {
        Some(s) if !s.is_empty() => {
            let show = s.len().min(visible);
            format!("{}...", &s[..show])
        }
        _ => "(not set)".to_string(),
    }
}

// ─── Update ─────────────────────────────────────────────────────────

const UPDATE_REPO: &str = "siderakis/bisque-tools-cli";

fn cmd_update(cli: &Cli, profile: Option<&BisqueProfile>) -> Result<()> {
    let current_version = env!("CARGO_PKG_VERSION");
    eprintln!("Current version: v{current_version}");
    eprintln!("Checking for updates...");

    // Fetch latest release
    let url = format!(
        "https://api.github.com/repos/{UPDATE_REPO}/releases/latest"
    );
    let resp = ureq::get(&url)
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", "bisque-cli")
        .call()
        .context("Failed to check for updates")?;

    let body: Value = serde_json::from_str(
        &resp.into_string().context("Failed to read response")?,
    )?;

    let tag = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .context("Missing tag_name in release")?;

    let latest_version = tag.trim_start_matches('v');

    if latest_version == current_version {
        eprintln!("Already up to date (v{current_version}).");
        return Ok(());
    }

    eprintln!("Updating v{current_version} → v{latest_version}...");

    // Detect platform
    let target = detect_target()?;
    let asset_name = format!("bisque-{target}.tar.gz");

    // Find download URL from release assets
    let download_url = body
        .get("assets")
        .and_then(|v| v.as_array())
        .and_then(|assets| {
            assets.iter().find_map(|a| {
                let name = a.get("name")?.as_str()?;
                if name == asset_name {
                    a.get("browser_download_url")
                        .and_then(|u| u.as_str())
                        .map(String::from)
                } else {
                    None
                }
            })
        })
        .with_context(|| {
            format!("No release asset found for {asset_name}")
        })?;

    // Download to temp file
    let dl_resp = ureq::get(&download_url)
        .call()
        .context("Failed to download update")?;

    let mut tarball = Vec::new();
    dl_resp
        .into_reader()
        .read_to_end(&mut tarball)
        .context("Failed to read download")?;

    // Extract binary from tarball
    let decoder = flate2::read::GzDecoder::new(&tarball[..]);
    let mut archive = tar::Archive::new(decoder);
    let current_exe = std::env::current_exe()
        .context("Failed to determine current executable path")?;

    let tmp_path = std::env::temp_dir().join("bisque-update.tmp");

    let mut found = false;
    for entry in archive.entries().context("Failed to read archive")? {
        let mut entry = entry.context("Failed to read archive entry")?;
        let path = entry
            .path()
            .context("Failed to read entry path")?
            .to_path_buf();
        if path.file_name().and_then(|n| n.to_str()) == Some("bisque")
        {
            let mut file = fs::File::create(&tmp_path)
                .context("Failed to create temp file")?;
            io::copy(&mut entry, &mut file)
                .context("Failed to write binary")?;
            found = true;
            break;
        }
    }

    if !found {
        // Clean up temp file
        let _ = fs::remove_file(&tmp_path);
        bail!("Archive does not contain a 'bisque' binary");
    }

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            &tmp_path,
            fs::Permissions::from_mode(0o755),
        )
        .context("Failed to set permissions")?;
    }

    // Atomic swap
    fs::rename(&tmp_path, &current_exe).context(
        "Failed to replace binary. You may need to run with sudo.",
    )?;

    eprintln!("Updated to v{latest_version}.");

    // Auto-sync skills using the new binary
    let auth = config::resolve_auth(
        cli.user_id.as_deref(),
        cli.api_key.as_deref(),
        cli.base_url.as_deref(),
        profile,
    );
    if !auth.user_id.is_empty() && !auth.api_key.is_empty() {
        eprintln!("\nSyncing skills...");
        let exe = current_exe.to_string_lossy().to_string();
        let status = std::process::Command::new(&exe)
            .arg("sync")
            .status();
        match status {
            Ok(s) if s.success() => {}
            _ => eprintln!("Sync failed — run `bisque sync` manually."),
        }
    }

    Ok(())
}

fn detect_target() -> Result<String> {
    let os = if cfg!(target_os = "macos") {
        "apple-darwin"
    } else if cfg!(target_os = "linux") {
        "unknown-linux-gnu"
    } else {
        bail!("Unsupported OS for self-update")
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        bail!("Unsupported architecture for self-update")
    };

    Ok(format!("{arch}-{os}"))
}
