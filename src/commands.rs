use crate::api::ApiClient;
use crate::config::{self, BisqueConfig, BisqueProfile};
use crate::{Cli, Command, CORE_SKILL_DIR_NAME, GENERATED_SKILL_PREFIX};
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
    integration_id: Option<String>,
    web_integration_id: Option<String>,
    access: Option<String>,
    safe: Option<bool>,
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

    // 2. Show pairing code
    eprintln!("\nYour pairing code: {pairing_code}\n");
    eprintln!(
        "Press Enter to open the browser or visit:\n  {browser_url}\n"
    );
    eprintln!("(^C to quit)\n");

    // Wait for Enter
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;

    // Open browser (best-effort)
    let _ = open::that(browser_url);

    // 3. Poll for approval
    eprintln!("Waiting for confirmation...");
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

    match client.get_json("/agentBootstrap") {
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
    let result = client.get_json("/agentBootstrap")?;
    let tools = parse_tools(&result);

    if cli.json {
        let arr: Vec<Value> = tools
            .iter()
            .map(|t| serde_json::to_value(t).unwrap())
            .collect();
        print_json(&Value::Array(arr), !cli.raw);
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
    let result = client.get_json("/agentBootstrap")?;
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

    let result = client.post_json("/agentToolCall", &body)?;
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

    let result = client.get_json("/agentBootstrap")?;
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

    // Remove stale dirs
    let mut removed = Vec::new();
    for dir in &existing_dirs {
        if !current_dirs.contains(dir) {
            let _ = fs::remove_dir_all(skills_root.join(dir));
            removed.push(dir.clone());
        }
    }

    // Update core SKILL.md with unconnected integrations discovery section
    update_core_skill_with_discovery(&result, &bisque_api_dir)?;

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
    if added.is_empty() && updated.is_empty() && removed.is_empty() {
        eprintln!("  (no changes)");
    }

    Ok(())
}

/// Reads the core SKILL.md template and appends a section listing
/// unconnected integrations with `bisque connect` commands.
fn update_core_skill_with_discovery(
    bootstrap: &Value,
    bisque_api_dir: &Path,
) -> Result<()> {
    let skill_md_path = bisque_api_dir.join("SKILL.md");
    if !skill_md_path.exists() {
        return Ok(());
    }

    let status = match bootstrap
        .get("integrationStatus")
        .and_then(|v| v.as_object())
    {
        Some(s) => s,
        None => return Ok(()),
    };

    // Integrations that are not agent-connectable (skip them)
    let skip = [
        "google",
        "imessage",
        "weather",
    ];

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
            let slug = integration_slug(key);
            let label = integration_label(key).to_string();
            unconnected.push((slug, label));
        }
    }

    unconnected.sort_by(|a, b| a.0.cmp(&b.0));

    // Read existing SKILL.md
    let mut content =
        fs::read_to_string(&skill_md_path).unwrap_or_default();

    // Remove any previous discovery section
    if let Some(pos) =
        content.find("\n## Available integrations (not connected)")
    {
        content.truncate(pos);
    }

    // Append discovery section if there are unconnected integrations
    if !unconnected.is_empty() {
        content.push_str(
            "\n## Available integrations (not connected)\n\n",
        );
        content.push_str(
            "The following integrations are available but the user hasn't connected them yet.\n",
        );
        content.push_str(
            "If any of these would help with the current task, let the user know and offer to\n",
        );
        content.push_str(
            "open the setup page:\n\n",
        );
        content.push_str(
            "| Integration | Connect command |\n",
        );
        content.push_str(
            "|-------------|----------------|\n",
        );
        for (slug, label) in &unconnected {
            content.push_str(&format!(
                "| {label} | `bisque connect {slug}` |\n"
            ));
        }
        content.push_str(
            "\nRunning `bisque connect <name>` opens the browser to the OAuth/setup page.\n",
        );
        content.push_str(
            "After the user completes setup, run `bisque sync` to pull in the new tools.\n",
        );
    }

    fs::write(&skill_md_path, content)?;
    Ok(())
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

    let result = match client.get_json("/agentBootstrap") {
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
        if let Some(ref id) = tool.integration_id {
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
                    {
                        dirs.insert(name);
                    }
                }
            }
        }
    }
    dirs
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
  "summary": "Human-readable result summary.",
  "data": {{}}
}}
```

- `status` is `"succeeded"`, `"failed"`, or `"denied"`.
- `summary` is always present — use it when relaying results to the user.
- `data` is optional structured output.

## Guidelines

- Use the `summary` field to respond to the user — do not dump raw `data`
  unless they ask for details.
- If a tool returns `"denied"`, tell the user the integration may need
  re-authorization at bisque.tools.
- Prefer `--raw` when parsing output programmatically.
"#,
        label = group.label,
        count = group.tools.len(),
        plural = plural,
        tool_rows = tool_rows,
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
        let summary = result
            .get("summary")
            .and_then(|v| v.as_str())
            .or_else(|| {
                result
                    .get("result")
                    .and_then(|r| r.get("summary"))
                    .and_then(|v| v.as_str())
            });
        match summary {
            Some(s) => println!("{s}"),
            None => print_json(result, !cli.raw),
        }
        return;
    }

    if let Some(ref field_path) = cli.field {
        match get_nested_field(result, field_path) {
            Some(value) => {
                if let Some(s) = value.as_str() {
                    println!("{s}");
                } else {
                    print_json(value, !cli.raw);
                }
            }
            None => {
                eprintln!("Field \"{field_path}\" not found.");
                std::process::exit(1);
            }
        }
        return;
    }

    print_json(result, !cli.raw);
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
