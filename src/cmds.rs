use std::io::{Read, Write};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::store::{Liveness, Store, StreamMeta};
use crate::{group, timefmt, units};

fn liveness_label(meta: &StreamMeta) -> &'static str {
    match meta.liveness() {
        Liveness::Running => "running",
        Liveness::Done => "done",
        Liveness::Died => "died",
    }
}

pub fn ls(store: &Store) -> Result<()> {
    let mut streams = store.list()?;
    streams.sort_by_key(|m| std::cmp::Reverse(m.started_at_ms));
    if streams.is_empty() {
        println!("no streams captured yet — pipe something through `lgt` first");
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
    for m in &streams {
        // Finalized streams carry their true counters; anything else (still
        // running, or killed before finalizing) is measured from the data
        // file itself.
        let (bytes, lines) = match m.liveness() {
            Liveness::Done => (m.bytes, m.lines),
            _ => store.data_stats(&m.id),
        };
        rows.push([
            m.id.clone(),
            m.name.clone().unwrap_or_else(|| "-".to_string()),
            m.group_name.clone().unwrap_or_else(|| m.group_id.clone()),
            liveness_label(m).to_string(),
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

pub fn cat(store: &Store, reference: &str, follow: bool) -> Result<()> {
    let meta = store.resolve(reference)?;
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

pub fn clean(store: &Store, all: bool) -> Result<()> {
    let mut removed = 0u64;
    for meta in store.list()? {
        if all || meta.liveness() != Liveness::Running {
            store.remove_stream(&meta.id)?;
            removed += 1;
        }
    }
    group::prune_stale_claims(store, timefmt::now_ms());
    println!("removed {removed} stream(s)");
    Ok(())
}

pub fn path(store: &Store) -> Result<()> {
    println!("{}", store.root().display());
    Ok(())
}
