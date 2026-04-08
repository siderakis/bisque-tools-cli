use clap::{Parser, Subcommand};

mod api;
mod commands;
mod config;

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

fn main() {
    let cli = Cli::parse();
    if let Err(e) = commands::run(cli) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
