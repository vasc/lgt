use std::collections::{HashMap, HashSet};
use std::thread::JoinHandle;

use crate::store::{Liveness, MetaStamp, Store, StreamMeta};

pub struct Entry {
    pub meta: StreamMeta,
    pub liveness: Liveness,
    /// Data size: live from the data file while running, else from meta.
    pub bytes: u64,
    stamp: Option<MetaStamp>,
}

/// The TUI's in-memory view of one shelf. Finished streams (done or died)
/// can never change on disk, so their metadata is read exactly once. A
/// refresh costs one directory listing, plus for each *running* stream a
/// `stat` of its meta, a pid probe and a `stat` of its data — independent
/// of how much finished history the shelf holds.
#[derive(Default)]
pub struct Catalog {
    entries: HashMap<String, Entry>,
    generation: u64,
    meta_reads: u64,
    /// Background threads recording newly noticed deaths on disk.
    recorders: Vec<JoinHandle<()>>,
}

/// Below this many new streams, loading in parallel is not worth the threads.
pub const PARALLEL_LOAD_MIN: usize = 64;

/// An entry plus whether its capture was just found dead (in memory only).
type Loaded = (Entry, bool);

fn load_entry(store: &Store, id: &str) -> Option<Loaded> {
    let stamp = store.meta_stamp(id);
    let meta = store.read_meta(id).ok()?;
    Some(entry_for(store, meta, stamp))
}

/// Load the metadata of streams the catalog has not seen yet. The cost is
/// dominated by per-file syscall latency, not CPU, so large batches (the
/// initial load) are spread across threads.
fn load_entries(store: &Store, ids: Vec<String>) -> Vec<(String, Option<Loaded>)> {
    if ids.len() < PARALLEL_LOAD_MIN {
        return ids
            .into_iter()
            .map(|id| {
                let entry = load_entry(store, &id);
                (id, entry)
            })
            .collect();
    }
    // More threads mostly contend in the kernel: on a 5000-stream store,
    // 16 threads burned ~0.49s of CPU to load, 4 threads ~0.22s, with the
    // load staying under budget either way.
    let threads = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(4)
        .clamp(2, 4);
    let chunk = ids.len().div_ceil(threads);
    std::thread::scope(|scope| {
        let handles: Vec<_> = ids
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .map(|id| (id.clone(), load_entry(store, id)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    })
}

/// Build an entry from what is on disk, without writing. A capture found
/// dead is shown as died right away (its size is one `stat`); recording the
/// death (which counts the stream's lines) is left to a background thread.
fn entry_for(store: &Store, meta: StreamMeta, stamp: Option<MetaStamp>) -> Loaded {
    let observed = store.observe(meta);
    let meta = observed.meta;
    let liveness = meta.liveness();
    let bytes = match liveness {
        Liveness::Running => store.data_len(&meta.id),
        _ => meta.bytes,
    };
    let entry = Entry {
        meta,
        liveness,
        bytes,
        stamp,
    };
    (entry, observed.died_unrecorded)
}

impl Catalog {
    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.get(id)
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Bumped whenever a stream appears, disappears, or changes metadata or
    /// liveness — i.e. whenever the row tree must be rebuilt. Byte counts of
    /// running streams change without bumping it.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn any_running(&self) -> bool {
        self.entries
            .values()
            .any(|e| e.liveness == Liveness::Running)
    }

    #[cfg(test)]
    pub fn meta_reads(&self) -> u64 {
        self.meta_reads
    }

    /// Bring the catalog in line with the shelf. Returns true when the
    /// generation changed.
    pub fn refresh(&mut self, store: &Store) -> bool {
        let Ok(dir) = std::fs::read_dir(store.streams_dir()) else {
            let changed = !self.entries.is_empty();
            self.entries.clear();
            if changed {
                self.generation += 1;
            }
            return changed;
        };
        let mut seen: HashSet<String> = HashSet::with_capacity(self.entries.len());
        let mut new_ids: Vec<String> = Vec::new();
        let mut deaths: Vec<String> = Vec::new();
        let mut changed = false;
        for dirent in dir.flatten() {
            let Ok(id) = dirent.file_name().into_string() else {
                continue;
            };
            match self.entries.get_mut(&id) {
                Some(entry) if entry.liveness != Liveness::Running => {}
                Some(entry) => {
                    let stamp = store.meta_stamp(&id);
                    let meta = if stamp == entry.stamp {
                        entry.meta.clone()
                    } else {
                        match store.read_meta(&id) {
                            Ok(meta) => {
                                self.meta_reads += 1;
                                changed = true;
                                meta
                            }
                            Err(_) => entry.meta.clone(),
                        }
                    };
                    let (refreshed, died) = entry_for(store, meta, stamp);
                    if refreshed.liveness != entry.liveness {
                        changed = true;
                    }
                    if died {
                        deaths.push(id.clone());
                    }
                    *entry = refreshed;
                }
                None => new_ids.push(id.clone()),
            }
            seen.insert(id);
        }
        for (id, loaded) in load_entries(store, new_ids) {
            // A stream directory whose meta is not published yet is picked up
            // on a later refresh.
            if let Some((entry, died)) = loaded {
                self.meta_reads += 1;
                if died {
                    deaths.push(id.clone());
                }
                self.entries.insert(id, entry);
                changed = true;
            }
        }
        let before = self.entries.len();
        self.entries.retain(|id, _| seen.contains(id));
        if self.entries.len() != before {
            changed = true;
        }
        self.record_deaths(store, deaths);
        if changed {
            self.generation += 1;
        }
        changed
    }

    /// Write the `died` status for captures found dead, off the UI thread:
    /// it counts each stream's lines, which for a large stream takes long.
    fn record_deaths(&mut self, store: &Store, ids: Vec<String>) {
        self.recorders.retain(|handle| !handle.is_finished());
        if ids.is_empty() {
            return;
        }
        let store = store.clone();
        self.recorders.push(std::thread::spawn(move || {
            for id in ids {
                // Best effort: if the stream was removed or moved meanwhile,
                // there is nothing left to record.
                store.record_death(&id);
            }
        }));
    }

    #[cfg(test)]
    pub fn wait_for_recorders(&mut self) {
        for handle in self.recorders.drain(..) {
            handle.join().unwrap();
        }
    }

    #[cfg(test)]
    pub fn from_metas(metas: Vec<StreamMeta>) -> Self {
        let mut catalog = Catalog::default();
        for meta in metas {
            let liveness = meta.liveness();
            let bytes = meta.bytes;
            catalog.entries.insert(
                meta.id.clone(),
                Entry {
                    meta,
                    liveness,
                    bytes,
                    stamp: None,
                },
            );
        }
        catalog.generation = 1;
        catalog
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
