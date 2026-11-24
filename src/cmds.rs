use std::io::{Read, Write};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::store::{Liveness, Shelf, Store, StreamMeta};
use crate::{timefmt, units};

fn liveness_label(liveness: Liveness) -> &'static str {
    match liveness {
        Liveness::Running => "running",
        Liveness::Done => "done",
        Liveness::Died => "died",
    }
}

pub fn ls(store: &Store) -> Result<()> {
    let mut streams = store.list_observed()?;
    streams.sort_by_key(|o| std::cmp::Reverse(o.meta.started_at_ms));
    if streams.is_empty() {
        match store.shelf() {
            Shelf::Active => {
                println!("no streams captured yet — pipe something through `lgt` first");
            }
            Shelf::Archive => println!("the archive is empty"),
        }
        return Ok(());
    }
    let mut rows = vec![[
        "ID".to_string(),
        "NAME".to_string(),
        "GROUP".to_string(),
        "STATUS".to_string(),
        "STARTED".to_string(),
        "LINES".to_string(),
        "BYTES".to_string(),
    ]];
    for observed in &streams {
        let m = &observed.meta;
        let liveness = m.liveness();
        // Counters in meta are final for finished streams. A dead capture
        // seen for the first time has its death recorded now (counting its
        // lines once, so later listings are cheap); a running stream is
        // measured from its data file.
        let (bytes, lines) = if observed.died_unrecorded {
            match store.record_death(&m.id) {
                Some(recorded) => (recorded.bytes, recorded.lines),
                None => store.data_stats(&m.id),
            }
        } else if liveness == Liveness::Running {
            store.data_stats(&m.id)
        } else {
            (m.bytes, m.lines)
        };
        rows.push([
            m.id.clone(),
            m.name.clone().unwrap_or_else(|| "-".to_string()),
            m.group_name.clone().unwrap_or_else(|| m.group_id.clone()),
            liveness_label(liveness).to_string(),
            timefmt::clock(m.started_at_ms),
            lines.to_string(),
            units::human_bytes(bytes),
        ]);
    }
    let mut widths = [0usize; 7];
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row.iter()) {
            *w = (*w).max(cell.len());
        }
    }
    for row in &rows {
        let mut line = String::new();
        for (w, cell) in widths.iter().zip(row.iter()) {
            line.push_str(&format!("{cell:<width$}  ", width = w));
        }
        println!("{}", line.trim_end());
    }
    Ok(())
}

/// Resolve a reference for reading. On the active shelf, an exact stream id
/// that has been archived is still found, so ids printed by
/// `lgt ls --archived` can be read without repeating the flag. Names only
/// resolve on the shelf being queried.
fn resolve_for_reading(store: &Store, reference: &str) -> Result<(Store, StreamMeta)> {
    match store.resolve(reference) {
        Ok(meta) => Ok((store.clone(), meta)),
        Err(err) => {
            let archive = store.on(Shelf::Archive);
            if store.shelf() == Shelf::Active && archive.meta_path(reference).exists() {
                let meta = archive.read_meta(reference)?;
                return Ok((archive, meta));
            }
            Err(err)
        }
    }
}

pub fn cat(store: &Store, reference: &str, follow: bool) -> Result<()> {
    let (store, meta) = resolve_for_reading(store, reference)?;
    let path = store.data_path(&meta.id);
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    let mut file =
        std::fs::File::open(&path).with_context(|| format!("opening {}", path.display()))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut final_pass = false;
    loop {
        loop {
            let n = match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e).context("reading capture file"),
            };
            match stdout.write_all(&buf[..n]) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
                Err(e) => return Err(e).context("writing stdout"),
            }
        }
        let _ = stdout.flush();
        if !follow || final_pass {
            return Ok(());
        }
        let live = store
            .read_meta(&meta.id)
            .map(|m| m.liveness())
            .unwrap_or(Liveness::Died);
        if live != Liveness::Running {
            // The stream finished; its last bytes may have landed between our
            // read and the status check, so drain once more before stopping.
            final_pass = true;
            continue;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

pub fn rm(store: &Store, references: &[String], force: bool) -> Result<()> {
    // Resolve everything up front so a bad reference removes nothing.
    let mut targets = Vec::new();
    for reference in references {
        targets.push(store.resolve(reference)?);
    }
    if !force {
        for meta in &targets {
            if meta.liveness() == Liveness::Running {
                bail!(
                    "stream {} is still running; removing it would discard the rest of \
                     its capture — pass -f/--force to remove it anyway",
                    meta.display_name()
                );
            }
        }
    }
    for meta in &targets {
        store.remove_stream(&meta.id)?;
        println!("removed {}", meta.id);
    }
    Ok(())
}

pub fn path(store: &Store) -> Result<()> {
    println!("{}", store.root().display());
    Ok(())
}
