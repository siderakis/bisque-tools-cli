use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BisqueConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,
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

pub fn resolve_profile_name(cli_profile: Option<&str>, config: &Option<BisqueConfig>) -> String {
    cli_profile
        .map(String::from)
        .or_else(|| std::env::var("BISQUE_PROFILE").ok())
        .or_else(|| config.as_ref().and_then(|c| c.active_profile.clone()))
        .unwrap_or_else(|| "default".to_string())
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
