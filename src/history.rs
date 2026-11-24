//! `lgt clean`, `lgt archive`, `lgt unarchive`: deliberate history
//! management. Nothing here ever runs implicitly, and planning (including
//! every `--dry-run`) never writes.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};

use crate::retention::{self, Candidate, Limits};
use crate::store::{Liveness, Shelf, Store, StreamMeta};
use crate::{timefmt, units};

struct Planned {
    meta: StreamMeta,
    size: u64,
}

fn require_limits(command: &str, limits: &Limits) -> Result<()> {
    if limits.is_empty() {
        bail!(
            "`lgt {command}` needs a selection: an AGE (e.g. `lgt {command} 30d`), --keep N, \
             --max-size SIZE, or --everything to select every finished stream"
        );
    }
    Ok(())
}

/// Pick the streams on `store`'s shelf that `limits` select. Running streams
/// are only eligible when `include_running` is set.
fn plan(store: &Store, limits: &Limits, include_running: bool) -> Result<Vec<Planned>> {
    let now = timefmt::now_ms();
    let metas = store.list()?;
    let candidates: Vec<Candidate> = metas
        .iter()
        .map(|m| Candidate {
            id: m.id.clone(),
            at_ms: store.last_activity_ms(m),
            size: store.stream_size(&m.id),
            eligible: include_running || m.liveness() != Liveness::Running,
        })
        .collect();
    let selected = retention::select(&candidates, limits, now);
    let index_of: HashMap<&str, usize> = metas
        .iter()
        .enumerate()
        .map(|(i, m)| (m.id.as_str(), i))
        .collect();
    Ok(selected
        .iter()
        .filter_map(|id| index_of.get(id.as_str()))
        .map(|&i| Planned {
            meta: metas[i].clone(),
            size: candidates[i].size,
        })
        .collect())
}

fn print_plan(verb: &str, planned: &[Planned]) {
    let total: u64 = planned.iter().map(|p| p.size).sum();
    println!(
        "would {verb} {} stream(s) ({})",
        planned.len(),
        units::human_bytes(total)
    );
    for p in planned {
        println!(
            "  {}  {}  {}  {}",
            p.meta.id,
            p.meta.name.as_deref().unwrap_or("-"),
            timefmt::clock(p.meta.started_at_ms),
            units::human_bytes(p.size)
        );
    }
}

/// Apply `step` to every planned stream, continuing past failures, then
/// report: `"<verb> N stream(s) (SIZE)"`, plus every failure on stderr.
fn apply(planned: &[Planned], verb: &str, mut step: impl FnMut(&str) -> Result<()>) -> Result<()> {
    let mut done = 0usize;
    let mut size = 0u64;
    let mut failures = Vec::new();
    for p in planned {
        match step(&p.meta.id) {
            Ok(()) => {
                done += 1;
                size += p.size;
            }
            Err(err) => failures.push(format!("{}: {err:#}", p.meta.id)),
        }
    }
    println!("{verb} {done} stream(s) ({})", units::human_bytes(size));
    for failure in &failures {
        eprintln!("lgt: {failure}");
    }
    if !failures.is_empty() {
        bail!("{} of {} stream(s) failed", failures.len(), planned.len());
    }
    Ok(())
}

/// Permanently delete streams from `store`'s shelf.
pub fn clean(store: &Store, limits: &Limits, include_running: bool, dry_run: bool) -> Result<()> {
    require_limits("clean", limits)?;
    let planned = plan(store, limits, include_running)?;
    if dry_run {
        print_plan("remove", &planned);
        return Ok(());
    }
    let result = apply(&planned, "removed", |id| store.remove_stream(id));
    if store.shelf() == Shelf::Active {
        crate::group::prune_stale_claims(store, timefmt::now_ms());
    }
    result
}

/// Move streams from the active shelf to the archive: finished ones, plus
/// running-looking ones with `include_running`.
pub fn archive(store: &Store, limits: &Limits, include_running: bool, dry_run: bool) -> Result<()> {
    if store.shelf() != Shelf::Active {
        bail!("`lgt archive` moves streams from the active shelf; drop --archived");
    }
    require_limits("archive", limits)?;
    let planned = plan(store, limits, include_running)?;
    if dry_run {
        print_plan("archive", &planned);
        return Ok(());
    }
    let archive = store.on(Shelf::Archive);
    apply(&planned, "archived", |id| store.move_stream(id, &archive))?;
    println!("browse the archive with `lgt tui --archived`");
    Ok(())
}

/// Move streams back from the archive to the active shelf: those named by
/// `references` (all resolved before anything moves) plus, with `since_ms`,
/// every archived stream active within that window.
pub fn unarchive(
    store: &Store,
    references: &[String],
    since_ms: Option<i64>,
    dry_run: bool,
) -> Result<()> {
    if store.shelf() != Shelf::Active {
        bail!("`lgt unarchive` always restores from the archive; drop --archived");
    }
    if references.is_empty() && since_ms.is_none() {
        bail!("name the streams to restore, or select them with --since AGE");
    }
    let archive = store.on(Shelf::Archive);
    let mut seen: HashSet<String> = HashSet::new();
    let mut planned = Vec::new();
    for reference in references {
        let meta = archive.resolve(reference)?;
        if seen.insert(meta.id.clone()) {
            planned.push(meta);
        }
    }
    if let Some(window) = since_ms {
        let now = timefmt::now_ms();
        let cutoff = now.saturating_sub(window);
        let mut recent: Vec<StreamMeta> = archive
            .list()?
            .into_iter()
            .filter(|m| archive.last_activity_ms(m) >= cutoff)
            .collect();
        recent.sort_by_key(|m| m.started_at_ms);
        planned.extend(recent.into_iter().filter(|m| seen.insert(m.id.clone())));
    }
    let planned: Vec<Planned> = planned
        .into_iter()
        .map(|meta| Planned {
            size: archive.stream_size(&meta.id),
            meta,
        })
        .collect();
    if dry_run {
        print_plan("restore", &planned);
        return Ok(());
    }
    apply(&planned, "restored", |id| archive.move_stream(id, store))
}
