use crate::sync::errors::{Code, SyncError};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Root marker filename.
pub const WORKSPACE_MARKER: &str = "bisque.yaml";
pub const STATE_DIR: &str = ".bisque";
pub const STATE_DB_FILE: &str = "state.db";
pub const INTEGRATIONS_DIR: &str = "integrations";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceManifest {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub name: Option<String>,
}

fn default_version() -> u32 {
    1
}

impl Default for WorkspaceManifest {
    fn default() -> Self {
        Self {
            version: 1,
            name: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub manifest: WorkspaceManifest,
}

impl Workspace {
    pub fn state_dir(&self) -> PathBuf {
        self.root.join(STATE_DIR)
    }

    pub fn state_db_path(&self) -> PathBuf {
        self.state_dir().join(STATE_DB_FILE)
    }

    pub fn integrations_dir(&self) -> PathBuf {
        self.root.join(INTEGRATIONS_DIR)
    }

    /// Discover providers configured under `integrations/<provider>/provider.yaml`.
    pub fn providers(&self) -> Result<Vec<ProviderConfig>> {
        let mut out = Vec::new();
        let dir = self.integrations_dir();
        if !dir.is_dir() {
            return Ok(out);
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let provider_name = entry.file_name().to_string_lossy().to_string();
            let provider_yaml = entry.path().join("provider.yaml");
            if !provider_yaml.is_file() {
                continue;
            }
            let raw = fs::read_to_string(&provider_yaml)?;
            let cfg: ProviderConfig = serde_yaml::from_str(&raw).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to parse {}: {}",
                    provider_yaml.display(),
                    e
                )
            })?;
            // If provider.yaml's explicit name disagrees with the dir, trust the dir.
            let mut cfg = cfg;
            cfg.provider = provider_name;
            cfg.root = entry.path();
            out.push(cfg);
        }
        out.sort_by(|a, b| a.provider.cmp(&b.provider));
        Ok(out)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default)]
    pub provider: String,
    #[serde(skip)]
    pub root: PathBuf,
}

impl ProviderConfig {
    pub fn kind_dir(&self, kind: &str) -> PathBuf {
        // Templates live in `templates/` (kind-singular "template" maps to plural dir).
        let plural = pluralize(kind);
        self.root.join(plural)
    }

    pub fn list_resource_files(&self, kind: &str) -> Result<Vec<PathBuf>> {
        let dir = self.kind_dir(kind);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut files: Vec<PathBuf> = WalkDir::new(&dir)
            .max_depth(1)
            .into_iter()
            .filter_map(|r| r.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .filter(|p| {
                matches!(
                    p.extension().and_then(|s| s.to_str()),
                    Some("yaml") | Some("yml")
                )
            })
            .collect();
        files.sort();
        Ok(files)
    }
}

fn pluralize(kind: &str) -> String {
    if kind.ends_with('s') {
        kind.to_string()
    } else {
        format!("{kind}s")
    }
}

/// Walk up from `start` looking for a `bisque.yaml`. Returns None if not found.
pub fn find_workspace_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join(WORKSPACE_MARKER).is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Discover and load the workspace rooted at the current working directory.
pub fn load_workspace() -> Result<Workspace, SyncError> {
    let cwd = std::env::current_dir().map_err(|e| {
        SyncError::new(
            Code::NoWorkspace,
            format!("Could not read current directory: {e}"),
            "Run from inside a directory that contains (or is descended from) a bisque.yaml.",
        )
    })?;
    let root = find_workspace_root(&cwd).ok_or_else(|| {
        SyncError::new(
            Code::NoWorkspace,
            "No bisque.yaml found in the current directory or any parent.",
            "Run `bisque-sync init` to create one.",
        )
    })?;
    let manifest_path = root.join(WORKSPACE_MARKER);
    let raw = fs::read_to_string(&manifest_path).map_err(|e| {
        SyncError::new(
            Code::NoWorkspace,
            format!("Failed to read {}: {}", manifest_path.display(), e),
            "Check file permissions and re-run.",
        )
    })?;
    let manifest: WorkspaceManifest = serde_yaml::from_str(&raw).map_err(|e| {
        SyncError::new(
            Code::YamlParse,
            format!("Failed to parse {}: {}", manifest_path.display(), e),
            "Ensure bisque.yaml is valid YAML (version: 1, optional name: <string>).",
        )
    })?;
    Ok(Workspace { root, manifest })
}
