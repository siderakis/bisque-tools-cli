use clap::{Parser, Subcommand};

mod api;
mod commands;
mod config;

pub const DEFAULT_BASE_URL: &str = "https://bisque.tools";
pub const GENERATED_SKILL_PREFIX: &str = "bisque-";
pub const CORE_SKILL_DIR_NAME: &str = "bisque-api";

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

    /// Compact JSON output
    #[arg(long, global = true)]
    pub raw: bool,

    /// Output as JSON (tools command)
    #[arg(long, global = true)]
    pub json: bool,

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

    /// Install SKILL.md + sync for a target agent
    Init {
        /// Target agent: claude-code, codex
        #[arg(long)]
        agent: Option<String>,

        /// Explicit skills directory
        #[arg(long)]
        skills_dir: Option<String>,

        /// Overwrite existing SKILL.md
        #[arg(long)]
        force: bool,
    },

    /// List available tools
    Tools,

    /// Full bootstrap payload
    Bootstrap,

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

    /// Generate per-integration skill directories
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
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = commands::run(cli) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
