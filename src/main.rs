mod capture;
mod cmds;
mod group;
mod proc;
mod store;
mod timefmt;
mod tui;
mod units;

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use store::Store;

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
        lgt cat -f step2                     follow a capture from another terminal"
)]
struct Cli {
    /// Identifier for this capture, e.g. `cmd | lgt build`; reference it later
    /// with `lgt cat NAME` or find it in the TUI
    name: Option<String>,

    /// Link this capture into a named group across pipelines (env: LGT_GROUP)
    #[arg(short = 'g', long, value_name = "NAME")]
    group: Option<String>,

    /// Keep capturing to the log after downstream closes the pipe
    /// (default: stop, like tee)
    #[arg(long)]
    drain: bool,

    /// Also write a copy of the capture to FILE (classic tee behavior);
    /// the primary log always lives in lgt's own data directory
    #[arg(short = 'o', long, value_name = "FILE")]
    out: Option<PathBuf>,

    /// Data directory (env: LGT_DATA_DIR; default: the platform data dir)
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
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
    /// Remove finished streams
    Clean {
        /// Also remove streams that are still running
        #[arg(long)]
        all: bool,
    },
    /// Print the data directory path
    Path,
}

fn data_root(flag: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(dir) = flag {
        return Ok(dir);
    }
    if let Ok(dir) = std::env::var("LGT_DATA_DIR")
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    let base = dirs::data_local_dir()
        .context("could not determine a data directory; pass --data-dir or set LGT_DATA_DIR")?;
    Ok(base.join("lgt"))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.command.is_some()
        && (cli.name.is_some() || cli.group.is_some() || cli.drain || cli.out.is_some())
    {
        // clap's own args_conflicts_with_subcommands would stop recognizing
        // subcommands after any parsed flag (even a global one), so this
        // conflict is enforced here instead.
        bail!(
            "capture options ([NAME], -g/--group, --drain, -o/--out) cannot be combined \
             with a subcommand"
        );
    }
    let store = Store::open(data_root(cli.data_dir)?)?;
    match cli.command {
        Some(Command::Tui) => tui::run(&store),
        Some(Command::Ls) => cmds::ls(&store),
        Some(Command::Cat { reference, follow }) => cmds::cat(&store, &reference, follow),
        Some(Command::Rm { references, force }) => cmds::rm(&store, &references, force),
        Some(Command::Clean { all }) => cmds::clean(&store, all),
        Some(Command::Path) => cmds::path(&store),
        None => {
            if std::io::stdin().is_terminal() {
                if let Some(name) = cli.name {
                    bail!(
                        "nothing is piped into `lgt {name}`; to capture, pipe data in \
                         (`cmd | lgt {name}`), or run `lgt` alone to browse"
                    );
                }
                tui::run(&store)
            } else {
                let outcome = capture::run(
                    &store,
                    capture::CaptureOpts {
                        name: cli.name,
                        group: cli.group,
                        drain: cli.drain,
                        out: cli.out.clone(),
                    },
                )?;
                if outcome == capture::CaptureOutcome::DownstreamClosed && !cli.drain {
                    // tee dies of SIGPIPE (status 141) when its reader goes
                    // away; report the same so `set -o pipefail` pipelines
                    // behave identically with lgt in place of tee.
                    std::process::exit(141);
                }
                Ok(())
            }
        }
    }
}
