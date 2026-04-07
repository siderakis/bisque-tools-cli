use crate::api::{ApiClient, ToolCallResponse};
use crate::config::{self, BisqueConfig, BisqueProfile};
use crate::{Cli, Command, GENERATED_SKILL_PREFIX};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

// ─── Types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SkillsResponse {
    skills: Vec<RenderedSkill>,
    core_skill: RenderedSkill,
    discovery_skill: Option<RenderedSkill>,
    #[serde(default)]
    skills_version: Option<String>,
    #[serde(default)]
    cli_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CliState {
    #[serde(skip_serializing_if = "Option::is_none")]
    skills_version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenderedSkill {
    directory_name: String,
    files: HashMap<String, String>,
}

// ─── Command dispatch ───────────────────────────────────────────────

pub fn run(cli: Cli) -> Result<()> {
    let config = config::load_config();
    let profile_name = config::resolve_profile_name(cli.profile.as_deref(), &config);

    // Validate profile exists (skip for login which creates profiles)
    if !matches!(cli.command, Command::Login) {
        if cli.profile.is_some() && config::get_profile(&config, &profile_name).is_none() {
            bail!(
                "Profile \"{}\" not found in {}",
                profile_name,
                config::config_path().display()
            );
        }
    }

    let profile = config::get_profile(&config, &profile_name).cloned();

    match &cli.command {
        Command::Login => cmd_login(&cli, &config, &profile_name, profile.as_ref()),
        Command::Doctor => cmd_doctor(&cli, profile.as_ref()),
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
        Command::Connect { integration } => cmd_connect(&cli, profile.as_ref(), integration),
        Command::ConfigOptions {
            provider,
            fields,
            context,
        } => cmd_config_options(
            &cli,
            profile.as_ref(),
            provider,
            fields.as_deref(),
            context.as_deref(),
        ),
        Command::SaveConfig {
            provider,
            key,
            value,
        } => cmd_save_config(&cli, profile.as_ref(), provider, key, value),
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
    let has_manual_flags = cli.user_id.is_some() || cli.api_key.is_some();

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

        return browser_login(base_url, config, profile_name, existing_profile);
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
    let session_body = match hostname::get() {
        Ok(name) => serde_json::json!({ "name": name.to_string_lossy() }).to_string(),
        Err(_) => "{}".to_string(),
    };
    let resp = ureq::post(&url)
        .set("Content-Type", "application/json")
        .send_string(&session_body)
        .context("Failed to create CLI session")?;

    let body: Value =
        serde_json::from_str(&resp.into_string().context("Failed to read response")?)?;

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
    let poll_url = format!("{base_url}/api/poll-cli-session?t={token}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);

    loop {
        if std::time::Instant::now() > deadline {
            bail!("Session expired. Run `bisque login` to try again.");
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
                let mut cfg = config.clone().unwrap_or_default();
                let profiles = cfg.profiles.get_or_insert_with(HashMap::new);
                let profile = BisqueProfile {
                    user_id: Some(user_id.clone()),
                    api_key: Some(api_key.clone()),
                    ..existing_profile.cloned().unwrap_or_default()
                };
                profiles.insert(profile_name.to_string(), profile);

                if cfg.active_profile.is_none() {
                    cfg.active_profile = Some("default".to_string());
                }

                config::save_config(&cfg)?;
                let path = config::config_path();
                eprintln!("\nDone! Credentials saved to {}", path.display());
                return Ok(());
            }
            "expired" => {
                bail!("Session expired. Run `bisque login` to try again.");
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
            eprintln!("  userId: {}", mask_str(existing.user_id.as_deref(), 8));
            eprintln!("  apiKey: {}", mask_str(existing.api_key.as_deref(), 12));
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
            bail!("Non-interactive mode requires --user-id and --api-key flags.");
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

    let client = ApiClient::new(base_url.to_string(), user_id, api_key);

    match client.get_json("/v1/toolboxes") {
        Ok(result) => {
            let count = result
                .get("providers")
                .and_then(|p| p.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            eprintln!("  Auth OK — {count} integration(s) available.");
        }
        Err(e) => {
            eprintln!("  Auth failed: {e}");
            eprintln!("  Credentials were saved. Check your user ID and API key.");
            std::process::exit(1);
        }
    }

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
    let client = ApiClient::new(auth.base_url, auth.user_id, auth.api_key);

    let raw_args = args_json
        .map(String::from)
        .unwrap_or_else(read_stdin_if_piped);

    let args: Value = if raw_args.is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        let parsed: Value = serde_json::from_str(&raw_args).context("Invalid JSON for args")?;
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

    let response = client.post_tool_call("/v1/tool-call", &body)?;
    match response {
        ToolCallResponse::Json(ref result) => {
            print_result(result, cli);
            if let Some(latest) = result.get("cliVersion").and_then(|v| v.as_str()) {
                check_cli_version(latest);
            }
        }
        ToolCallResponse::Binary { content_type, data } => {
            let stdout = io::stdout();
            if stdout.is_terminal() {
                // Interactive terminal — don't dump binary, show metadata
                let ext = mime_to_ext(&content_type);
                eprintln!(
                    "Binary response: {} ({} bytes, {})",
                    content_type,
                    data.len(),
                    ext,
                );
                eprintln!(
                    "Pipe to a file to save: bisque call {} --args '...' > output.{}",
                    tool_name, ext
                );
            } else {
                // Piped — write raw bytes to stdout
                let mut out = stdout.lock();
                out.write_all(&data)?;
                out.flush()?;
            }
        }
    }
    Ok(())
}

fn mime_to_ext(content_type: &str) -> &str {
    if content_type.contains("audio/mpeg") {
        "mp3"
    } else if content_type.contains("audio/wav") {
        "wav"
    } else if content_type.contains("audio/ogg") {
        "ogg"
    } else if content_type.contains("audio/flac") {
        "flac"
    } else if content_type.contains("image/png") {
        "png"
    } else if content_type.contains("image/jpeg") {
        "jpg"
    } else if content_type.contains("application/pdf") {
        "pdf"
    } else if content_type.contains("application/zip") {
        "zip"
    } else {
        "bin"
    }
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
    let client = ApiClient::new(auth.base_url, auth.user_id, auth.api_key);

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
    eprintln!("Skills root: {}", skills_root.display());

    // Fetch pre-rendered skills from server
    let result = client.get_json("/v1/skills")?;
    let response: SkillsResponse =
        serde_json::from_value(result).context("Failed to parse /v1/skills response")?;

    let existing_dirs = find_existing_generated_dirs(&skills_root);
    let mut current_dirs = HashSet::new();
    let mut added = Vec::new();
    let mut updated = Vec::new();

    // Write all skills (core + integrations + discovery)
    let mut all_skills = vec![response.core_skill];
    all_skills.extend(response.skills);
    if let Some(ds) = response.discovery_skill {
        all_skills.push(ds);
    }

    let mut unchanged = Vec::new();

    for skill in &all_skills {
        let dir_name = &skill.directory_name;
        current_dirs.insert(dir_name.clone());
        let dir_path = skills_root.join(dir_name);
        let existed = existing_dirs.contains(dir_name);

        // Check if any file content actually changed
        let mut changed = !existed;
        if existed {
            for (filename, content) in &skill.files {
                let file_path = dir_path.join(filename);
                match fs::read_to_string(&file_path) {
                    Ok(existing) if existing == *content => {}
                    _ => {
                        changed = true;
                        break;
                    }
                }
            }
        }

        if changed {
            fs::create_dir_all(&dir_path)?;
            for (filename, content) in &skill.files {
                fs::write(dir_path.join(filename), content)?;
            }
        }

        if !existed {
            added.push(dir_name.clone());
        } else if changed {
            updated.push(dir_name.clone());
        } else {
            unchanged.push(dir_name.clone());
        }
    }

    // Remove stale bisque-* dirs not in response
    let mut removed = Vec::new();
    for dir in &existing_dirs {
        if !current_dirs.contains(dir) {
            let _ = fs::remove_dir_all(skills_root.join(dir));
            removed.push(dir.clone());
        }
    }

    // Summary
    let integration_count = all_skills.len().saturating_sub(1); // exclude core
    eprintln!("Synced {} integration(s):", integration_count);
    for name in &added {
        eprintln!("  + {name} (new)");
    }
    for name in &updated {
        eprintln!("  ~ {name} (updated)");
    }
    for name in &removed {
        eprintln!("  - {name} (removed)");
    }
    if !unchanged.is_empty() {
        eprintln!("  {} unchanged", unchanged.len());
    }
    if added.is_empty() && updated.is_empty() && removed.is_empty() {
        eprintln!("  (no changes)");
    }

    // Save skills version for staleness detection
    if let Some(ref version) = response.skills_version {
        let _ = save_skills_version(version);
    }

    // Check for CLI update
    if let Some(ref latest) = response.cli_version {
        check_cli_version(latest);
    }

    if !added.is_empty() || !updated.is_empty() || !removed.is_empty() {
        eprintln!("\nRestart your Claude Code session to pick up the changes.");
    }

    Ok(())
}

// ─── Doctor ─────────────────────────────────────────────────────────

fn cmd_doctor(cli: &Cli, profile: Option<&BisqueProfile>) -> Result<()> {
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
        println!("  BISQUE_API_KEY: {}", mask_str(Some(&auth.api_key), 12));
    }

    println!("  Target:        {}", auth.base_url);

    if auth.user_id.is_empty() || auth.api_key.is_empty() {
        println!("\nCannot check API connectivity without credentials.");
        std::process::exit(if ok { 0 } else { 1 });
    }

    // 2. API auth check via /v1/toolboxes
    println!("\nChecking API...");
    let client = ApiClient::new(auth.base_url, auth.user_id, auth.api_key);

    let result = match client.get_json("/v1/toolboxes") {
        Ok(r) => {
            println!("  Auth: OK");
            r
        }
        Err(e) => {
            println!("  Auth: FAILED — {e}");
            std::process::exit(1);
        }
    };

    // 3. Integration status from /v1/toolboxes response
    println!("\nIntegrations:");
    let mut tool_count = 0;
    let mut provider_count = 0;

    if let Some(providers) = result.get("providers").and_then(|v| v.as_array()) {
        let mut connected = Vec::new();
        let mut disconnected = Vec::new();

        for provider in providers {
            let label = provider
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let id = provider
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let is_connected = provider
                .get("connected")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            if is_connected {
                // Count tools from connected providers
                if let Some(toolboxes) = provider.get("toolboxes").and_then(|v| v.as_array()) {
                    for tb in toolboxes {
                        tool_count += tb.get("toolCount").and_then(|v| v.as_u64()).unwrap_or(0);
                    }
                }
                connected.push(label.to_string());
                provider_count += 1;
            } else {
                disconnected.push((label.to_string(), id.to_string()));
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
            for (name, id) in &disconnected {
                println!("    - {name} (bisque connect {id})");
            }
        }
    }

    // 4. Stale generated dirs
    if let Ok(skills_root) = resolve_skills_root(None, None) {
        let existing_dirs = find_existing_generated_dirs(&skills_root);

        // Expected dirs: bisque-api + one per connected provider
        // We can't know exact dir names without calling /v1/skills,
        // so just report dirs that exist on disk for awareness
        if !existing_dirs.is_empty() {
            println!("\n  Skill directories on disk:");
            for name in &existing_dirs {
                println!("    {name}");
            }
            println!("  Run `bisque sync` to refresh.");
        }
    }

    println!(
        "\nTools: {} available across {} integration(s)",
        tool_count, provider_count
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

// ─── Config Options ────────────────────────────────────────────────

fn cmd_config_options(
    cli: &Cli,
    profile: Option<&BisqueProfile>,
    provider: &str,
    fields: Option<&str>,
    context: Option<&str>,
) -> Result<()> {
    let auth = config::require_auth(
        cli.user_id.as_deref(),
        cli.api_key.as_deref(),
        cli.base_url.as_deref(),
        profile,
    )?;
    let client = ApiClient::new(auth.base_url, auth.user_id, auth.api_key);

    // Provider IDs are simple kebab-case strings (e.g. "google-analytics"),
    // field keys are alphanumeric — no encoding needed.
    let mut path = format!("/v1/config-options?providerId={provider}");
    if let Some(f) = fields {
        path.push_str(&format!("&fieldKeys={f}"));
    }
    if let Some(c) = context {
        // Context is JSON — percent-encode it for the query string.
        let encoded: String = c
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect();
        path.push_str(&format!("&context={encoded}"));
    }

    let result = client.get_json(&path)?;
    print_result(&result, cli);
    Ok(())
}

// ─── Save Config ───────────────────────────────────────────────────

fn cmd_save_config(
    cli: &Cli,
    profile: Option<&BisqueProfile>,
    provider: &str,
    key: &str,
    value: &str,
) -> Result<()> {
    let auth = config::require_auth(
        cli.user_id.as_deref(),
        cli.api_key.as_deref(),
        cli.base_url.as_deref(),
        profile,
    )?;
    let client = ApiClient::new(auth.base_url, auth.user_id, auth.api_key);

    let body = serde_json::json!({
        "providerId": provider,
        "values": { key: value }
    });

    let result = client.post_json("/v1/save-config", &body)?;
    print_result(&result, cli);
    Ok(())
}

// ─── Connect ────────────────────────────────────────────────────────

/// Maps integration status keys to web UI URL paths.
fn integration_url_path(integration: &str) -> Option<String> {
    // Google services all share a single integrations page
    if integration.starts_with("google-") || integration == "google" {
        return Some("/integrations/google".to_string());
    }
    // All other providers use /integrations/{provider-id}
    Some(format!("/integrations/{integration}"))
}

fn cmd_connect(cli: &Cli, profile: Option<&BisqueProfile>, integration: &str) -> Result<()> {
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
            eprintln!("Unknown integration: \"{integration}\"\n");
            eprintln!("Available integrations:");
            eprintln!("  google, klaviyo, mailchimp, meta-ads,");
            eprintln!("  reddit-ads, stripe, tesla, x");
            eprintln!("\nGoogle sub-services (google-analytics, google-calendar, etc.)");
            eprintln!("all connect through the Google integration page.");
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

fn resolve_skills_root(skills_dir: Option<&str>, agent: Option<&str>) -> Result<PathBuf> {
    if let Some(dir) = skills_dir {
        return Ok(PathBuf::from(dir));
    }

    if let Some(agent) = agent {
        let home = dirs::home_dir().context("Cannot determine home directory")?;
        return match agent {
            "claude-code" => Ok(home.join(".claude").join("skills")),
            "codex" => {
                let codex_home = std::env::var("CODEX_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| home.join(".codex"));
                Ok(codex_home.join("skills"))
            }
            _ => {
                bail!("Unknown agent: {agent}. Use \"claude-code\" or \"codex\".")
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

    let home = dirs::home_dir().context("Cannot determine home directory")?;
    if home.join(".claude").exists() {
        return Ok(home.join(".claude").join("skills"));
    }
    if home.join(".codex").exists() {
        return Ok(home.join(".codex").join("skills"));
    }

    bail!("Could not determine skills directory. Use --skills-dir or --agent.");
}

fn find_existing_generated_dirs(skills_root: &Path) -> HashSet<String> {
    let mut dirs = HashSet::new();
    if let Ok(entries) = fs::read_dir(skills_root) {
        for entry in entries.flatten() {
            if let Ok(ft) = entry.file_type() {
                if ft.is_dir() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with(GENERATED_SKILL_PREFIX) {
                        dirs.insert(name);
                    }
                }
            }
        }
    }
    dirs
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

fn get_nested_field<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
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

// ─── Version helpers ────────────────────────────────────────────────

fn check_cli_version(latest: &str) {
    let current = env!("CARGO_PKG_VERSION");
    if latest != current {
        eprintln!(
            "bisque: a newer version is available (v{latest}). Run `bisque update` and restart your session."
        );
    }
}

fn state_file_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".bisque")
        .join("state.json")
}

fn load_state() -> Option<CliState> {
    let content = fs::read_to_string(state_file_path()).ok()?;
    serde_json::from_str(&content).ok()
}

fn save_skills_version(version: &str) -> Result<()> {
    let mut state = load_state().unwrap_or_default();
    state.skills_version = Some(version.to_string());
    let json = serde_json::to_string_pretty(&state)?;
    let path = state_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, format!("{json}\n"))?;
    Ok(())
}

// ─── Update ─────────────────────────────────────────────────────────

const UPDATE_REPO: &str = "siderakis/bisque-tools-cli";

fn cmd_update(cli: &Cli, profile: Option<&BisqueProfile>) -> Result<()> {
    let current_version = env!("CARGO_PKG_VERSION");
    eprintln!("Current version: v{current_version}");
    eprintln!("Checking for updates...");

    // Fetch latest release
    let url = format!("https://api.github.com/repos/{UPDATE_REPO}/releases/latest");
    let resp = ureq::get(&url)
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", "bisque-cli")
        .call()
        .context("Failed to check for updates")?;

    let body: Value =
        serde_json::from_str(&resp.into_string().context("Failed to read response")?)?;

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
        .with_context(|| format!("No release asset found for {asset_name}"))?;

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
    let current_exe =
        std::env::current_exe().context("Failed to determine current executable path")?;

    let tmp_path = std::env::temp_dir().join("bisque-update.tmp");

    let mut found = false;
    for entry in archive.entries().context("Failed to read archive")? {
        let mut entry = entry.context("Failed to read archive entry")?;
        let path = entry
            .path()
            .context("Failed to read entry path")?
            .to_path_buf();
        if path.file_name().and_then(|n| n.to_str()) == Some("bisque") {
            let mut file = fs::File::create(&tmp_path).context("Failed to create temp file")?;
            io::copy(&mut entry, &mut file).context("Failed to write binary")?;
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
        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o755))
            .context("Failed to set permissions")?;
    }

    // Atomic swap
    fs::rename(&tmp_path, &current_exe)
        .context("Failed to replace binary. You may need to run with sudo.")?;

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
        let status = std::process::Command::new(&exe).arg("sync").status();
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
