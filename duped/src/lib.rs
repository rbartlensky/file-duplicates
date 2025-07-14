//! This library can be used to find duplicated files across a list of preconfigured directories ("roots").
//!
//! # Examples
//!
//! ```no_run
//! use duped::{ContentLimit, Deduper, NoopFindHook};
//!
//! let deduper = Deduper::builder(vec!["./".into()]).build();
//! let stats = deduper.find(ContentLimit::no_limit(), NoopFindHook).unwrap();
//! ```

use std::{
    io,
    path::PathBuf,
    sync::{
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
};

pub use blake3;
use tracing::error;
use walkdir::WalkDir;

mod duplicates;
mod file;
mod hasher;
mod traits;

pub use duplicates::{DeduperResult, FileEntries, FileEntry};
use file::FilePath;
use hasher::ProgressiveHasher;
pub use traits::*;

/// File deduplicator.
#[derive(Debug)]
pub struct Deduper {
    inner: DeduperInner,
}

impl Deduper {
    /// Create a [`DeduperBuilder`].
    pub fn builder(roots: Vec<PathBuf>) -> DeduperBuilder {
        DeduperBuilder::new(roots)
    }

    /// Return the configured roots.
    pub fn roots(&self) -> &[PathBuf] {
        &self.inner.roots
    }

    /// Collect all files and their metadata into a vector based on a given filter.
    fn collect_files(
        &self,
        mut file_filter: impl DeduperFileFilter,
    ) -> (Vec<ProgressiveHasher>, bool) {
        let mut stopped = false;
        let mut files = vec![];
        'main: for root in &self.inner.roots {
            for entry in WalkDir::new(root) {
                let path = match entry {
                    Ok(p) => p.into_path(),
                    Err(e) => {
                        error!(error = %e, "io error occured while waking dirs");
                        continue;
                    }
                };
                if path.is_dir() || path.is_symlink() {
                    continue;
                }

                let file_path = match FilePath::try_new(path.clone()) {
                    Ok(md) => md,
                    Err(e) => {
                        error!(error = %e, path = %path.display(), "io error when reading metadata");
                        continue;
                    }
                };

                match file_filter.handle_file(file_path.path(), file_path.metadata()) {
                    FilterAction::Continue(FileAction::Exclude) => {}
                    FilterAction::Continue(FileAction::Include) => {
                        files.push(ProgressiveHasher::new(file_path));
                    }
                    FilterAction::Break(_) => {
                        stopped = true;
                        break 'main;
                    }
                }
            }
        }

        (files, stopped)
    }

    /// Finds and returns duplicated files on disk.
    pub fn find(
        &self,
        file_filter: impl DeduperFileFilter,
        find_hook: impl DeduperFindHook,
    ) -> io::Result<DeduperResult> {
        let hooks = Arc::new(find_hook) as Arc<dyn DeduperFindHook>;

        let (mut collected_files, stopped) = self.collect_files(file_filter);
        let collected_files_len = collected_files.len();

        if stopped || collected_files_len == 0 {
            return Ok(Default::default());
        }

        let num_threads = num_cpus::get();

        let (result_tx, result_rx) = mpsc::sync_channel(num_threads);
        let mut threads = Vec::with_capacity(num_threads);
        for i in 0..num_threads {
            let (thread_tx, thread_rx) = mpsc::sync_channel(1);
            let result_tx = result_tx.clone();
            let handle = std::thread::spawn(move || hasher_task(i, thread_rx, result_tx));
            threads.push((handle, thread_tx));
        }

        let (collector_tx, collector_rx) = mpsc::sync_channel(1);
        hooks.files_selected(collected_files_len);
        let collector = std::thread::spawn(move || {
            collect(collected_files_len, result_rx, collector_tx, hooks)
        });
        drop(result_tx);

        loop {
            let chunk_size = (collected_files.len() / threads.len()).max(1);
            let mut hashers_to_be_sent = Vec::with_capacity(threads.len());
            for _ in 0..threads.len() {
                hashers_to_be_sent.push(Vec::with_capacity(chunk_size));
            }
            let mut i = 0;
            while !collected_files.is_empty() {
                let size = chunk_size.min(collected_files.len());
                let hashers = collected_files.drain(..size);
                hashers_to_be_sent[i % threads.len()].extend(hashers);

                i += 1;
            }

            for (i, hashers) in hashers_to_be_sent.into_iter().enumerate() {
                if threads[i].1.send(hashers).is_err() {
                    panic!("thread died?");
                }
            }

            let Ok(files) = collector_rx.recv() else {
                todo!("handle collector dying");
            };

            // no more files to hash, so we can clean up and return
            if files.is_empty() {
                // XXX: why doesn't rust "drop in place" rx if I use `_`?
                for (t, rx) in threads {
                    drop(rx);
                    t.join().expect("failed to join with thread");
                }

                let mut duplicates = collector.join().expect("failed to join with collector");
                if stopped {
                    duplicates.set_partial();
                }

                return Ok(duplicates);
            } else {
                collected_files = files;
            }
        }
    }
}

#[derive(Debug)]
struct DeduperInner {
    /// Where to start the search from.
    roots: Vec<PathBuf>,
    /// If the size of the file is under `lower_limit` bytes, it is not taken
    /// into account.
    lower_limit: Option<u64>,
}

/// A builder for [`Deduper`].
pub struct DeduperBuilder {
    inner: DeduperInner,
}

impl DeduperBuilder {
    /// Create a new instance of the builder with a list of roots.
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self { inner: DeduperInner { roots, lower_limit: None } }
    }

    /// Set the lower file size limit, in bytes.
    ///
    /// Files that are smaller than `limit` will be skipped (not checked for duplication).
    pub fn lower_limit(mut self, limit: u64) -> Self {
        self.inner.lower_limit = Some(limit);

        self
    }

    /// Build a [`Deduper`].
    pub fn build(self) -> Deduper {
        Deduper { inner: self.inner }
    }
}

fn hasher_task(
    worker_id: usize,
    tasks: Receiver<Vec<ProgressiveHasher>>,
    tx: SyncSender<(usize, ProgressiveHasher, io::Result<()>)>,
) {
    while let Ok(hashers) = tasks.recv() {
        for mut hasher in hashers {
            let res = hasher.update();

            if tx.send((worker_id, hasher, res)).is_err() {
                error!("failed to send hash, quiting...");
                break;
            }
        }
    }
}

fn collect(
    mut responses: usize,
    rx: Receiver<(usize, ProgressiveHasher, io::Result<()>)>,
    rehash_files_tx: SyncSender<Vec<ProgressiveHasher>>,
    hooks: Arc<dyn DeduperFindHook>,
) -> DeduperResult {
    let mut duplicates = DeduperResult::default();

    while responses > 0 {
        let mut hasher_set = hasher::HasherSet::default();
        for _ in 0..responses {
            let Ok((_, hasher, res)) = rx.recv() else {
                break;
            };

            let (hash, done) = hasher.current_hash();

            if let Err(e) = res {
                error!(
                    error = %e,
                    path = %hasher.file_path().path().display(),
                    "failed to process file"
                );
                continue;
            } else if done {
                let entry = hasher.file_path().to_file_entry();
                hooks.entry_processed(hash, &entry);
                duplicates.add_entry(hash, entry);
            } else {
                hasher_set.insert(hasher);
            }
        }

        let (finished, hashers) = hasher_set.filter_unfinished_duplicates();
        for hasher in finished {
            let (hash, _) = hasher.current_hash();
            let entry = hasher.file_path().to_file_entry();
            hooks.entry_processed(hash, &entry);
            duplicates.add_entry(hash, entry);
        }
        responses = hashers.len();
        if rehash_files_tx.send(hashers).is_err() {
            error!("rehash_files channel is closed");
            break;
        }
    }

    duplicates
}
