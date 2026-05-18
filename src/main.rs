use clap::{Parser, Subcommand};

mod api;
mod commands;
mod commands_sync;
mod config;
mod sync;
mod upload;
mod validate;

pub const DEFAULT_BASE_URL: &str = "https://bisque.tools";
pub const GENERATED_SKILL_PREFIX: &str = "bisque-";

#[derive(Parser)]
#[command(
    name = "bisque",
    version,
    about = "Bisque CLI — manage integrations and execute tools"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Profile from ~/.bisque/config.json
    #[arg(long, global = true)]
    pub profile: Option<String>,

    /// API base URL
    #[arg(long, global = true)]
    pub base_url: Option<String>,

    /// User ID (overrides config/env)
    #[arg(long, global = true)]
    pub user_id: Option<String>,

    /// API key (overrides config/env)
    #[arg(long, global = true)]
    pub api_key: Option<String>,

    /// Pretty-print JSON output (default is compact)
    #[arg(long, global = true)]
    pub pretty: bool,

    /// Print only the summary field
    #[arg(long, global = true)]
    pub summary_only: bool,

    /// Extract a nested field (dot-separated path)
    #[arg(long, global = true)]
    pub field: Option<String>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Set up credentials (~/.bisque/config.json)
    Login,

    /// Execute a tool
    Call {
        /// Tool name
        tool_name: String,

        /// Tool arguments as JSON object
        #[arg(long = "args", value_name = "JSON")]
        args_json: Option<String>,

        /// Request correlation ID
        #[arg(long)]
        invocation_id: Option<String>,

        /// Skip the local JSON Schema check (forward args to the proxy as-is;
        /// server-side validation still runs)
        #[arg(long = "skip-schema-check")]
        skip_schema_check: bool,
    },

    /// Sync per-integration skill directories from server
    Sync {
        /// Target agent: claude-code, codex
        #[arg(long)]
        agent: Option<String>,

        /// Explicit skills directory
        #[arg(long)]
        skills_dir: Option<String>,
    },

    /// Check credentials, auth, and integration status
    Doctor,

    /// Open browser to connect an integration
    Connect {
        /// Integration name (e.g., google-analytics, klaviyo, reddit-ads)
        integration: String,
    },

    /// List available config options for an integration (e.g. GA4 properties)
    ConfigOptions {
        /// Provider name (e.g., google-analytics, google-firebase)
        provider: String,

        /// Specific field keys to resolve (comma-separated). Defaults to all.
        #[arg(long)]
        fields: Option<String>,

        /// Context JSON for dependent fields (e.g. '{"projectId":"my-project"}')
        #[arg(long)]
        context: Option<String>,
    },

    /// Save a config value for an integration
    SaveConfig {
        /// Provider name (e.g., google-analytics)
        provider: String,

        /// Config field key (e.g., property)
        key: String,

        /// Config field value (e.g., properties/123456)
        value: String,
    },

    /// Update bisque to the latest version
    Update,

    /// Initialize a .bisque.json workspace config in the current directory
    Init,

    /// Manage linked accounts for an integration
    Accounts {
        #[command(subcommand)]
        action: AccountsAction,
    },
}

#[derive(Subcommand)]
pub enum AccountsAction {
    /// List linked accounts for a provider
    List {
        /// Provider name (e.g., google-gmail, meta-ads)
        provider: String,
    },

    /// Set the default account for a provider
    SetDefault {
        /// Provider name
        provider: String,

        /// Account ID to set as default
        account_id: String,
    },

    /// Set a description for an account
    Describe {
        /// Provider name
        provider: String,

        /// Account ID
        account_id: String,

        /// Description of the account's purpose
        description: String,
    },
}

// ─── bisque-sync identity ─────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "bisque-sync",
    version,
    about = "Bisque Sync — declarative project state for SaaS integrations",
    disable_help_subcommand = true
)]
pub struct SyncCli {
    #[command(subcommand)]
    pub command: SyncCommand,

    /// Profile from ~/.bisque/config.json
    #[arg(long, global = true)]
    pub profile: Option<String>,

    /// API base URL
    #[arg(long, global = true)]
    pub base_url: Option<String>,

    /// User ID (overrides config/env)
    #[arg(long, global = true)]
    pub user_id: Option<String>,

    /// API key (overrides config/env)
    #[arg(long, global = true)]
    pub api_key: Option<String>,

    /// Emit machine-readable JSON instead of human output
    #[arg(long, global = true)]
    pub json: bool,
}

#[derive(Subcommand)]
pub enum SyncCommand {
    /// Scaffold bisque.yaml + .bisque/ + CLAUDE.md stanza in the current directory
    Init {
        /// Do not write/append to CLAUDE.md
        #[arg(long)]
        no_claude_md: bool,
    },

    /// Import remote resources into YAML + state.db
    Import {
        /// Provider name (e.g. klaviyo)
        provider: String,
        /// Resource kind (e.g. templates). Defaults to all kinds the provider supports.
        kind: Option<String>,
    },

    /// Show pending changes
    Plan,

    /// Apply pending changes
    Apply {
        /// Print what would be called, skip the actual API calls
        #[arg(long)]
        dry_run: bool,
        /// Skip interactive confirmation (required when stdin is not a TTY)
        #[arg(long)]
        auto_approve: bool,
    },

    /// Render a managed resource (preview output bytes without applying)
    Render {
        /// Resource name (e.g. customer_at_risk_reminder)
        resource: String,
    },

    /// Print a workspace snapshot (providers, resources, state, pending plan summary)
    Explain,

    /// List managed resources with current/desired state
    Ls {
        /// Filter by provider (e.g. klaviyo)
        provider: Option<String>,
        /// Filter by kind (e.g. templates)
        kind: Option<String>,
    },

    /// Print the JSON Schema for a resource kind's YAML shape
    Schema {
        /// Provider name (e.g. klaviyo)
        provider: String,
        /// Resource kind (e.g. template). If omitted, lists available kinds for the provider.
        kind: Option<String>,
    },

    /// Verify workspace integrity, auth, render dependencies, known quirks
    Doctor,

    /// Print a help topic: workflow | schema | troubleshooting | <provider> | <provider> <kind>
    Help {
        /// One or two words: the topic, optionally a sub-kind (e.g. `klaviyo template`)
        topic: Vec<String>,
    },

    /// RESERVED — MCP server over stdio (returns E_NOT_IMPLEMENTED in prototype)
    Mcp,
}

fn main() {
    let argv0 = std::env::args().next().unwrap_or_default();
    let is_sync = std::path::Path::new(&argv0)
        .file_name()
        .map(|n| n == "bisque-sync")
        .unwrap_or(false);

    let result = if is_sync {
        let cli = SyncCli::parse();
        commands_sync::run(cli)
    } else {
        let cli = Cli::parse();
        commands::run(cli)
    };

    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
