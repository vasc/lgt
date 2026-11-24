//! Command-line definitions.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::{retention, units};

#[derive(Parser)]
#[command(
    name = "lgt",
    version,
    about = "lgt — a smarter tee: capture piped data, then browse and watch it live",
    long_about = "lgt is an observability-minded tee replacement.\n\
        Piped input is passed straight through to stdout while a copy is kept\n\
        in a local store. Run `lgt` in a terminal to open the TUI and browse\n\
        or live-tail captures, including ones still being written.\n\n\
        Instances in the same shell pipeline (cmd1 | lgt | cmd2 | lgt) link\n\
        automatically; use -g/--group or LGT_GROUP to link captures manually.",
    after_help = "Examples:\n  \
        make build 2>&1 | lgt build          capture (and pass through) under a name\n  \
        cmd1 | lgt step1 | cmd2 | lgt step2  auto-linked pipeline instances\n  \
        lgt                                  open the TUI (when stdin is a terminal)\n  \
        lgt cat -f step2                     follow a capture from another terminal\n  \
        lgt archive 24h                      move streams older than 24h to the archive\n  \
        lgt tui --archived                   browse the archive\n  \
        lgt clean --archived 90d             delete archived streams older than 90 days"
)]
pub struct Cli {
    /// Identifier for this capture, e.g. `cmd | lgt build`; reference it later
    /// with `lgt cat NAME` or find it in the TUI
    pub name: Option<String>,

    /// Link this capture into a named group across pipelines (env: LGT_GROUP)
    #[arg(short = 'g', long, value_name = "NAME")]
    pub group: Option<String>,

    /// Keep capturing to the log after downstream closes the pipe
    /// (default: stop, like tee)
    #[arg(long)]
    pub drain: bool,

    /// Also write a copy of the capture to FILE (classic tee behavior);
    /// the primary log always lives in lgt's own data directory
    #[arg(short = 'o', long, value_name = "FILE")]
    pub out: Option<PathBuf>,

    /// Data directory (env: LGT_DATA_DIR; default: the platform data dir)
    #[arg(long, global = true, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,

    /// Operate on the archive instead of the active streams (tui, ls, cat,
    /// rm, clean; rejected elsewhere)
    #[arg(long, global = true)]
    pub archived: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

fn parse_age(text: &str) -> Result<i64, String> {
    units::parse_duration(text).map_err(|e| e.to_string())
}

fn parse_size(text: &str) -> Result<u64, String> {
    units::parse_size(text).map_err(|e| e.to_string())
}

/// Selection shared by `clean` and `archive`. At least one is required; each
/// given limit is independent, so a stream is selected when it violates any
/// of them.
#[derive(Args)]
pub struct LimitArgs {
    /// Select streams whose last activity is older than AGE (e.g. 90m, 24h,
    /// 30d, 2w, 3d12h)
    #[arg(value_name = "AGE", value_parser = parse_age)]
    pub older_than: Option<i64>,
    /// Keep only the newest N streams, selecting the rest
    #[arg(long, value_name = "N")]
    pub keep: Option<usize>,
    /// Select the oldest streams until the total size fits (e.g. 500M, 5G)
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    pub max_size: Option<u64>,
    /// Select every finished stream
    #[arg(long)]
    pub everything: bool,
    /// Show what would be selected without changing anything
    #[arg(long)]
    pub dry_run: bool,
}

impl LimitArgs {
    fn is_unset(&self) -> bool {
        self.limits().is_empty() && !self.dry_run
    }

    pub fn limits(&self) -> retention::Limits {
        retention::Limits {
            older_than_ms: self.older_than,
            keep: self.keep,
            max_size: self.max_size,
            everything: self.everything,
        }
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// Browse and watch captured streams (default when stdin is a terminal)
    Tui,
    /// List captured streams
    Ls,
    /// Print a stream's captured data (a name resolves to its newest stream)
    Cat {
        /// Stream id or name
        reference: String,
        /// Keep printing as the stream grows, until it finishes
        #[arg(short = 'f', long)]
        follow: bool,
    },
    /// Remove captured streams by id or name
    Rm {
        /// Stream ids or names
        #[arg(required = true)]
        references: Vec<String>,
        /// Remove streams even if they are still running
        #[arg(short = 'f', long)]
        force: bool,
    },
    /// Permanently delete finished streams selected by AGE / --keep /
    /// --max-size / --everything
    Clean {
        #[command(flatten)]
        limits: LimitArgs,
        /// Also delete selected streams that are still running
        #[arg(long)]
        include_running: bool,
    },
    /// Move finished streams selected by AGE / --keep / --max-size /
    /// --everything to the archive, out of default listings and the TUI
    Archive {
        #[command(flatten)]
        limits: LimitArgs,
        /// Also archive selected streams that still look running (e.g. a
        /// capture silent for longer than AGE, or recorded in a container
        /// instance that no longer exists)
        #[arg(long)]
        include_running: bool,
    },
    /// Move archived streams back to the active streams
    Unarchive {
        /// Archived stream ids or names
        references: Vec<String>,
        /// Also restore every archived stream active within AGE (e.g. 2d)
        #[arg(long, value_name = "AGE", value_parser = parse_age)]
        since: Option<i64>,
        /// Show what would be restored without changing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Print the data directory path
    Path,
}

impl Command {
    /// Given without any argument of its own (global flags aside) — the
    /// shape of a capture whose name collides with a subcommand.
    pub fn is_bare(&self) -> bool {
        match self {
            Command::Tui | Command::Ls | Command::Path => true,
            Command::Clean {
                limits,
                include_running,
            } => !include_running && limits.is_unset(),
            Command::Archive {
                limits,
                include_running,
            } => !include_running && limits.is_unset(),
            Command::Cat { .. } | Command::Rm { .. } | Command::Unarchive { .. } => false,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Command::Tui => "tui",
            Command::Ls => "ls",
            Command::Cat { .. } => "cat",
            Command::Rm { .. } => "rm",
            Command::Clean { .. } => "clean",
            Command::Archive { .. } => "archive",
            Command::Unarchive { .. } => "unarchive",
            Command::Path => "path",
        }
    }
}
