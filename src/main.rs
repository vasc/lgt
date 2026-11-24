mod capture;
mod cli;
mod cmds;
mod group;
mod history;
mod meta;
mod names;
mod proc;
mod retention;
mod settle;
mod stdin_guard;
mod store;
mod timefmt;
mod tui;
mod units;

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;

use cli::{Cli, Command};
use store::{Shelf, Store};

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
    let root = Store::open(data_root(cli.data_dir)?)?;
    let store = if cli.archived {
        root.on(Shelf::Archive)
    } else {
        root
    };
    if let Some(command) = &cli.command {
        stdin_guard::refuse_piped_stdin(command.name(), command.is_bare())?;
    }
    match cli.command {
        Some(Command::Tui) => tui::run(&store),
        Some(Command::Ls) => cmds::ls(&store),
        Some(Command::Cat { reference, follow }) => cmds::cat(&store, &reference, follow),
        Some(Command::Rm { references, force }) => cmds::rm(&store, &references, force),
        Some(Command::Clean {
            limits,
            include_running,
        }) => history::clean(&store, &limits.limits(), include_running, limits.dry_run),
        Some(Command::Archive {
            limits,
            include_running,
        }) => history::archive(&store, &limits.limits(), include_running, limits.dry_run),
        Some(Command::Unarchive {
            references,
            since,
            dry_run,
        }) => history::unarchive(&store, &references, since, dry_run),
        Some(Command::Path) => {
            if cli.archived {
                bail!("`lgt path` prints the data directory; --archived does not apply");
            }
            cmds::path(&store)
        }
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
                if cli.archived {
                    bail!(
                        "--archived applies to browsing commands; captures always go to the active streams"
                    );
                }
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
