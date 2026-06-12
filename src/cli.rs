use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "aiz", version, about, long_about = None)]
pub struct Cli {
    #[arg(default_value = "", help = "Free-text search plus keyword filters")]
    pub query: String,

    #[arg(short = 'a', long = "agent", help = "Filter by agent id")]
    pub agent: Option<String>,

    #[arg(
        short = 'd',
        long = "directory",
        help = "Filter by working directory substring"
    )]
    pub directory: Option<String>,

    #[arg(
        long = "no-tui",
        help = "List matching sessions instead of opening the TUI"
    )]
    pub no_tui: bool,

    #[arg(long = "list", help = "List sessions and exit")]
    pub list: bool,

    #[arg(
        long = "rebuild",
        help = "Drop and reconstruct the index from agent files"
    )]
    pub rebuild: bool,

    #[arg(long = "stats", help = "Show the statistics dashboard and exit")]
    pub stats: bool,

    #[arg(
        long = "yolo",
        help = "Launch with the agent's skip-permission flags where supported"
    )]
    pub yolo: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "Fork an existing session into a fresh one")]
    Fork {
        #[arg(long = "to", help = "Target agent for a translated fork")]
        to: Option<String>,

        #[arg(long = "target-cwd", help = "Working directory for the generated fork")]
        target_cwd: Option<std::path::PathBuf>,

        #[arg(
            long = "dry-run",
            help = "Show what would happen without writing or launching"
        )]
        dry_run: bool,

        #[arg(long = "no-launch", help = "Create the fork but do not launch it")]
        no_launch: bool,

        #[arg(
            long = "no-register-codex-index",
            help = "Skip Codex SQLite registration for Codex targets"
        )]
        no_register_codex_index: bool,

        #[arg(help = "Session selector, either raw id or agent:id")]
        session_id: String,
    },

    #[command(about = "Manage aiz configuration")]
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    #[command(about = "Manage global launch permission behavior")]
    Permission {
        #[command(subcommand)]
        action: PermissionAction,
    },

    #[command(about = "Manage locally hidden sessions")]
    Hidden {
        #[command(subcommand)]
        action: HiddenAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum PermissionAction {
    #[command(about = "Print the current launch permission policy")]
    Get,

    #[command(about = "Set launch permission policy: normal, yolo, or inherit")]
    Set { value: String },

    #[command(about = "Reset launch permission policy so the TUI asks again")]
    Reset,
}

#[derive(Debug, Subcommand)]
pub enum HiddenAction {
    #[command(about = "List locally hidden sessions")]
    List,

    #[command(about = "Unhide one locally hidden session")]
    Unhide {
        #[arg(help = "Hidden session selector, either raw id or agent:id")]
        session_id: String,
    },

    #[command(about = "Unhide every locally hidden session")]
    Reset,
}
