use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BisqueConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profiles: Option<HashMap<String, BisqueProfile>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BisqueProfile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

pub struct AuthInfo {
    pub user_id: String,
    pub api_key: String,
    pub base_url: String,
}

pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".bisque")
        .join("config.json")
}

pub fn load_config() -> Option<BisqueConfig> {
    let content = fs::read_to_string(config_path()).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn save_config(config: &BisqueConfig) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("Failed to create ~/.bisque directory")?;
    }
    let json = serde_json::to_string_pretty(config)?;
    fs::write(&path, format!("{json}\n")).context("Failed to write config file")?;
    Ok(())
}

/// Project-local workspace config (`.bisque.json`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

/// Walk up from `cwd` looking for `.bisque.json`. Returns the parsed config
/// and the directory it was found in. Returns an error if the file exists but
/// cannot be read or parsed (to avoid silently falling back to the wrong account).
pub fn find_workspace_config() -> Result<Option<(WorkspaceConfig, PathBuf)>> {
    let mut dir = match std::env::current_dir() {
        Ok(d) => d,
        Err(_) => return Ok(None),
    };
    loop {
        let candidate = dir.join(".bisque.json");
        if candidate.is_file() {
            let content = fs::read_to_string(&candidate)
                .with_context(|| format!("Failed to read {}", candidate.display()))?;
            let ws: WorkspaceConfig = serde_json::from_str(&content)
                .with_context(|| format!("Invalid JSON in {}", candidate.display()))?;
            return Ok(Some((ws, dir)));
        }
        if !dir.pop() {
            return Ok(None);
        }
    }
}

pub fn resolve_profile_name(
    cli_profile: Option<&str>,
    config: &Option<BisqueConfig>,
) -> Result<String> {
    // 1. Explicit flag
    if let Some(p) = cli_profile {
        return Ok(p.to_string());
    }
    // 2. Environment variable
    if let Ok(p) = std::env::var("BISQUE_PROFILE") {
        return Ok(p);
    }
    // 3. Workspace config (.bisque.json)
    if let Some((ws, _)) = find_workspace_config()? {
        if let Some(p) = ws.profile {
            return Ok(p);
        }
    }
    // 4. If exactly one profile exists, use it
    let profile_names = sorted_profile_names(config);
    match profile_names.len() {
        0 => Ok("default".to_string()),
        1 => Ok(profile_names[0].clone()),
        _ => {
            // Multiple profiles: prompt interactively or error
            if io::stdin().is_terminal() {
                interactive_profile_picker(config, &profile_names)
            } else {
                bail!(
                    "Multiple profiles found ({}). Specify one with --profile or BISQUE_PROFILE.",
                    profile_names.join(", ")
                );
            }
        }
    }
}

/// Return sorted profile names from config, or empty vec.
pub fn sorted_profile_names(config: &Option<BisqueConfig>) -> Vec<String> {
    config
        .as_ref()
        .and_then(|c| c.profiles.as_ref())
        .map(|p| {
            let mut names: Vec<String> = p.keys().cloned().collect();
            names.sort();
            names
        })
        .unwrap_or_default()
}

/// Interactive arrow-key profile picker for terminals.
fn interactive_profile_picker(
    config: &Option<BisqueConfig>,
    names: &[String],
) -> Result<String> {
    eprintln!("Multiple profiles found. Select one:\n");
    for (i, name) in names.iter().enumerate() {
        let email = config
            .as_ref()
            .and_then(|c| c.profiles.as_ref())
            .and_then(|p| p.get(name))
            .and_then(|p| p.email.as_deref())
            .unwrap_or("");
        if email.is_empty() {
            eprintln!("  {}) {name}", i + 1);
        } else {
            eprintln!("  {}) {name} — {email}", i + 1);
        }
    }
    eprintln!();

    eprint!("Profile [1-{}]: ", names.len());
    io::stderr().flush().ok();

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("Failed to read input")?;
    let input = input.trim();

    // Accept number or name
    if let Ok(num) = input.parse::<usize>() {
        if num >= 1 && num <= names.len() {
            return Ok(names[num - 1].clone());
        }
        bail!("Invalid selection: {num}");
    }
    if names.contains(&input.to_string()) {
        return Ok(input.to_string());
    }
    bail!("Unknown profile: \"{input}\"");
}

/// Find a profile name that already has the given user ID.
pub fn find_profile_by_user_id(config: &Option<BisqueConfig>, user_id: &str) -> Option<String> {
    config
        .as_ref()
        .and_then(|c| c.profiles.as_ref())
        .and_then(|profiles| {
            profiles.iter().find_map(|(name, p)| {
                if p.user_id.as_deref() == Some(user_id) {
                    Some(name.clone())
                } else {
                    None
                }
            })
        })
}

pub fn get_profile<'a>(
    config: &'a Option<BisqueConfig>,
    profile_name: &str,
) -> Option<&'a BisqueProfile> {
    config
        .as_ref()
        .and_then(|c| c.profiles.as_ref())
        .and_then(|p| p.get(profile_name))
}

pub fn resolve_auth(
    cli_user_id: Option<&str>,
    cli_api_key: Option<&str>,
    cli_base_url: Option<&str>,
    profile: Option<&BisqueProfile>,
) -> AuthInfo {
    let user_id = first_non_empty(&[
        cli_user_id.map(String::from),
        std::env::var("BISQUE_USER_ID").ok(),
        profile.and_then(|p| p.user_id.clone()),
    ]);

    let api_key = first_non_empty(&[
        cli_api_key.map(String::from),
        std::env::var("BISQUE_API_KEY").ok(),
        profile.and_then(|p| p.api_key.clone()),
    ]);

    let base_url = first_non_empty(&[
        cli_base_url.map(String::from),
        std::env::var("BISQUE_BASE_URL").ok(),
        profile.and_then(|p| p.base_url.clone()),
    ])
    .unwrap_or_else(|| crate::DEFAULT_BASE_URL.to_string());

    AuthInfo {
        user_id: user_id.unwrap_or_default(),
        api_key: api_key.unwrap_or_default(),
        base_url: base_url.trim_end_matches('/').to_string(),
    }
}

pub fn require_auth(
    cli_user_id: Option<&str>,
    cli_api_key: Option<&str>,
    cli_base_url: Option<&str>,
    profile: Option<&BisqueProfile>,
) -> Result<AuthInfo> {
    let auth = resolve_auth(cli_user_id, cli_api_key, cli_base_url, profile);
    if auth.user_id.is_empty() {
        bail!("Missing user ID. Run `bisque login` or set BISQUE_USER_ID.");
    }
    if auth.api_key.is_empty() {
        bail!("Missing API key. Run `bisque login` or set BISQUE_API_KEY.");
    }
    Ok(auth)
}

fn first_non_empty(options: &[Option<String>]) -> Option<String> {
    for opt in options {
        if let Some(ref s) = opt {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}
