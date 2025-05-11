//! This library can be used to find duplicated files across a list of preconfigured directories ("roots").
//!
//! # Examples
//!
//! ```no_run
//! use duped::{ContentLimit, Deduper, NoopStopper, NoopFindHook};
//!
//! let deduper = Deduper::builder(vec!["./".into()]).build();
//! let stats = deduper.find(NoopStopper, ContentLimit::no_limit(), NoopFindHook).unwrap();
//! ```

use blake3::Hash;
use duplicates::FileEntry;
use filetime::FileTime;
use std::{
    collections::VecDeque,
    fs::File,
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
};
use walkdir::WalkDir;

#[cfg(feature = "sqlite")]
mod db;
mod duplicates;
mod traits;

#[cfg(feature = "sqlite")]
pub use db::{Entry, HashDb};
pub use duplicates::{DeduperResult, FileEntries};
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

    /// Return the configured database path.
    pub fn db_path(&self) -> Option<&Path> {
        self.inner.db_path.as_deref()
    }

    /// Finds and returns duplicated files on disk.
    pub fn find(
        &self,
        mut stopper: impl DeduperStop,
        mut file_filter: impl DeduperFileFilter,
        find_hook: impl DeduperFindHook,
    ) -> io::Result<DeduperResult> {
        let hooks = Arc::new(find_hook) as Arc<dyn DeduperFindHook>;

        // TODO: what's a good minimum number?
        let num_threads = num_cpus::get().min(16);
        let fds = number_of_fds_available(num_threads)?;

        let (tx, rx) = mpsc::sync_channel(fds);
        let mut threads = VecDeque::with_capacity(num_threads);
        for _ in 0..num_threads {
            let (thread_tx, thread_rx) = mpsc::sync_channel(fds / num_threads);
            let _db_path = self.inner.db_path.clone();
            let tx = tx.clone();
            let handle = std::thread::spawn(move || {
                #[cfg(feature = "sqlite")]
                if let Some(db) = _db_path {
                    hasher_task_with_caching(db, thread_rx, tx)
                } else {
                    Ok(hasher_task(thread_rx, tx))
                }
                #[cfg(not(feature = "sqlite"))]
                Result::<(), Error>::Ok(hasher_task(thread_rx, tx))
            });
            threads.push_back((handle, thread_tx));
        }
        let hooks_ = Arc::clone(&hooks);
        let collector = std::thread::spawn(|| collect(rx, hooks_));
        drop(tx);

        // whether a user action stopped the main loop
        let mut stopped = false;
        let mut next_worker = 0;
        'main: for root in &self.inner.roots {
            for entry in WalkDir::new(root) {
                if stopper.should_stop() {
                    stopped = true;
                    break 'main;
                }

                let mut path = match entry {
                    Ok(p) => p.into_path(),
                    Err(e) => {
                        eprintln!("io error occured: {}", e);
                        continue;
                    }
                };
                if path.is_dir() || path.is_symlink() {
                    continue;
                }
                let md = match path.metadata() {
                    Ok(md) => md,
                    Err(e) => {
                        eprintln!("io error when reading metadata {}: {}", path.display(), e);
                        continue;
                    }
                };

                if !file_filter.include_file(&path, &md) {
                    continue;
                }

                let mtime = FileTime::from_last_modification_time(&md);

                let mut file = match File::open(&path) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("failed to open {}: {}", path.display(), e);
                        continue;
                    }
                };

                'outer: loop {
                    for _ in 0..threads.len() {
                        // we want to have each thread doing something, hence why we go
                        // round-robin
                        let tx = &threads[next_worker].1;
                        next_worker = (next_worker + 1) % num_threads;
                        match tx.try_send((path, file, mtime)) {
                            Ok(()) => break 'outer,
                            // if a thread crashed, we continue on, since we'll
                            // see the error after we process all files
                            Err(mpsc::TrySendError::Full((p, f, _)))
                            | Err(mpsc::TrySendError::Disconnected((p, f, _))) => {
                                path = p;
                                file = f;
                            }
                        }
                    }
                    std::thread::yield_now();
                }
            }
        }
        // XXX: why doesn't rust "drop in place" rx if I use `_`?
        for (t, rx) in threads {
            drop(rx);
            t.join().expect("failed to join with thread").expect("db operation failed");
        }

        let mut duplicates = collector.join().expect("failed to join with collector");
        if stopped {
            duplicates.set_partial();
        }

        Ok(duplicates)
    }
}

fn number_of_fds_available(num_threads: usize) -> io::Result<usize> {
    // give some leeway so that we don't hit the limit by accident
    #[cfg(unix)]
    {
        let fds = rlimit::getrlimit(rlimit::Resource::NOFILE)?.0 as usize - 4 * num_threads;

        return Ok(fds);
    }
    #[cfg(windows)]
    {
        let fds = rlimit::getmaxstdio() as usize - 4 * num_threads;

        return Ok(fds);
    }
}

#[derive(Debug)]
struct DeduperInner {
    /// Where to start the search from.
    roots: Vec<PathBuf>,
    /// If the size of the file is under `lower_limit` bytes, it is not taken
    /// into account.
    lower_limit: Option<u64>,
    /// Where to store the hash database.
    db_path: Option<PathBuf>,
}

/// A builder for [`Deduper`].
pub struct DeduperBuilder {
    inner: DeduperInner,
}

impl DeduperBuilder {
    /// Create a new instance of the builder with a list of roots.
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self { inner: DeduperInner { roots, lower_limit: None, db_path: None } }
    }

    /// Set the lower file size limit, in bytes.
    ///
    /// Files that are smaller than `limit` will be skipped (not checked for duplication).
    pub fn lower_limit(mut self, limit: u64) -> Self {
        self.inner.lower_limit = Some(limit);

        self
    }

    /// Set the sqlite database path.
    ///
    /// If the path doesn't exist, the deduper will initialize an new sqlite database at that path.
    pub fn db_path(mut self, db_path: PathBuf) -> Self {
        self.inner.db_path = Some(db_path);

        self
    }

    /// Build a [`Deduper`].
    pub fn build(self) -> Deduper {
        Deduper { inner: self.inner }
    }
}

fn hash_file(file: File) -> io::Result<(u64, Hash)> {
    // blake3 docs suggest a 16 KiB buffer for best performance
    let mut reader = BufReader::with_capacity(16 * 1024 * 1024, file);
    let mut hasher = blake3::Hasher::new();
    let mut size = 0;
    loop {
        let data = reader.fill_buf()?;
        let len = data.len();
        if len == 0 {
            return Ok((size, hasher.finalize()));
        } else {
            hasher.update(data);
            reader.consume(len);
            size += len as u64;
        }
    }
}

fn hasher_task(
    tasks: Receiver<(PathBuf, File, FileTime)>,
    tx: SyncSender<(PathBuf, io::Result<(u64, Hash)>)>,
) {
    while let Ok((path, file, _)) = tasks.recv() {
        let res = hash_file(file);

        if tx.send((path, res)).is_err() {
            eprintln!("failed to send hash, quiting...");
            break;
        }
    }
}

#[derive(Debug)]
enum Error {
    #[cfg(feature = "sqlite")]
    #[allow(dead_code)]
    Sqlite(rusqlite::Error),
}

#[cfg(feature = "sqlite")]
impl From<rusqlite::Error> for Error {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sqlite(value)
    }
}

#[cfg(feature = "sqlite")]
fn hasher_task_with_caching(
    db: PathBuf,
    tasks: Receiver<(PathBuf, File, FileTime)>,
    tx: SyncSender<(PathBuf, io::Result<(u64, Hash)>)>,
) -> Result<(), Error> {
    use db::retry_on_busy;

    // each task has a connection to our db
    let db = db::HashDb::try_new(db)?;
    while let Ok((path, file, mtime)) = tasks.recv() {
        let res = match retry_on_busy(|| db.select(&path))? {
            // we found a matching file, so we don't need to compute the hash
            Some(entry) if entry.mtime == mtime.unix_seconds() => Ok((entry.size, entry.hash)),
            // we need to compute the hash and update our db
            _ => {
                let res = hash_file(file);
                if let Ok((size, hash)) = res {
                    let entry = db::Entry { path: &path, mtime: mtime.unix_seconds(), size, hash };
                    retry_on_busy(|| db.insert(&entry))?;
                    res
                } else {
                    res
                }
            }
        };
        if tx.send((path, res)).is_err() {
            eprintln!("failed to send hash, quiting...");
            break;
        }
    }
    Ok(())
}

fn collect(
    rx: Receiver<(PathBuf, io::Result<(u64, Hash)>)>,
    hooks: Arc<dyn DeduperFindHook>,
) -> DeduperResult {
    let mut duplicates = DeduperResult::default();
    while let Ok((path, res)) = rx.recv() {
        let (size, hash) = match res {
            Ok(h) => h,
            Err(e) => {
                eprintln!("failed to read from {}: {}", path.display(), e);
                continue;
            }
        };
        let entry = FileEntry::new(path, size);
        hooks.entry_processed(hash, &entry);
        duplicates.add_entry(hash, entry);
    }

    duplicates
}
