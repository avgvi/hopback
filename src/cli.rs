use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "hopback", version, about, long_about = None)]
pub struct Cli {
    #[arg(
        default_value = "",
        help = "Search text plus keyword filters, or `.` to resume the newest session of the current folder"
    )]
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
        long = "json",
        help = "Print matching sessions as one JSON object and exit"
    )]
    pub json: bool,

    #[arg(
        long = "limit",
        value_name = "N",
        conflicts_with = "all",
        value_parser = clap::value_parser!(u64).range(1..),
        help = "Maximum sessions to return (default: 50 for --json, 200 for --list)"
    )]
    pub limit: Option<u64>,

    #[arg(long = "offset", value_name = "N", help = "Skip this many matches")]
    pub offset: Option<usize>,

    #[arg(long = "all", help = "Return every match from the requested offset")]
    pub all: bool,

    #[arg(
        long = "no-refresh",
        global = true,
        help = "Serve the existing index without scanning agent files"
    )]
    pub no_refresh: bool,

    #[arg(
        long = "include-hidden",
        help = "Include sessions hidden in hopback in --json and --list output"
    )]
    pub include_hidden: bool,

    #[arg(
        long = "include-messages",
        help = "Include the conversation text of each session in --json output"
    )]
    pub include_messages: bool,

    #[arg(
        long = "rebuild",
        help = "Drop and reconstruct the index from agent files"
    )]
    pub rebuild: bool,

    #[arg(long = "stats", help = "Show the statistics dashboard and exit")]
    pub stats: bool,

    #[arg(
        long = "agent-context",
        help = "Print instructions for coding agents that want to use hopback, and exit"
    )]
    pub agent_context: bool,

    #[arg(
        long = "print",
        help = "Print the folder and the resume command instead of running it"
    )]
    pub print: bool,

    #[arg(
        long = "yolo",
        global = true,
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

        #[arg(
            long = "json",
            help = "Print the fork as one JSON object and do not launch it"
        )]
        json: bool,

        #[arg(help = "Session selector, either raw id or agent:id")]
        session_id: String,
    },

    #[command(about = "Pin a session to the top of the list")]
    Pin {
        #[command(flatten)]
        target: SessionArg,
    },

    #[command(about = "Remove a session's pin")]
    Unpin {
        #[command(flatten)]
        target: SessionArg,
    },

    #[command(about = "Give a session a title inside hopback")]
    Rename {
        #[command(flatten)]
        target: SessionArg,

        #[arg(help = "The new title", required_unless_present = "reset")]
        title: Option<String>,

        #[arg(
            long = "reset",
            conflicts_with = "title",
            help = "Remove the local title and show the agent's own again"
        )]
        reset: bool,
    },

    #[command(about = "Hide a session from hopback")]
    Hide {
        #[command(flatten)]
        target: SessionArg,
    },

    #[command(about = "Show a hidden session again")]
    Unhide {
        #[command(flatten)]
        target: SessionArg,
    },

    #[command(about = "Print a shell completion script")]
    Completions {
        #[arg(value_enum, help = "The shell to generate for")]
        shell: clap_complete::Shell,

        #[arg(
            long = "command",
            value_name = "NAME",
            value_parser = ["hopback", "hb"],
            help = "Complete this command name (default: the name hopback was started as)"
        )]
        command: Option<String>,
    },

    #[command(about = "Print the man page in roff format")]
    Man,

    #[command(about = "Manage hopback configuration")]
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

/// The session a metadata command acts on, and how to report the result.
#[derive(Debug, clap::Args)]
pub struct SessionArg {
    #[arg(help = "Session selector, either raw id or agent:id")]
    pub session: String,

    #[arg(
        long = "json",
        help = "Print the session as one JSON object after the change"
    )]
    pub json: bool,
}
