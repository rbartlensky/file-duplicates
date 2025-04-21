//! This library can be used to find duplicated files starting from a particular
//! root directory. To use it, please look over the following docs: [find],
//! [Params], and [Stats].
//!
//! # Examples
//!
//! ```no_run
//! use file_duplicates::{find, Params};
//!
//! let params = Params { lower_limit: 0, roots: vec!["./".into()], db: "test.db".into() };
//! let stats = find(&params).unwrap();
//! ```

use blake3::Hash;
use filetime::FileTime;
use std::{
    collections::{
        hash_map::Entry::{Occupied, Vacant},
        HashMap, VecDeque,
    },
    fs::File,
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender},
};
use walkdir::WalkDir;

mod db;
pub use db::{Entry, HashDb};

use crate::db::retry_on_busy;

/// Used to configure the [find] function.
#[derive(Debug)]
pub struct Params {
    /// If the size of the file is under `lower_limit` bytes, it is not taken
    /// into account.
    lower_limit: u64,
    /// Where to start the search from.
    roots: Vec<PathBuf>,
    /// Where to store the hash database.
    db: PathBuf,
}

impl Params {
    /// Create a new instance of [`Params`].
    ///
    /// # Arguments
    ///
    /// * `lower_limit` - If the size of the file is under `lower_limit` bytes, it is not taken
    ///                   into account.
    /// * `root` - Where to start the search from.
    /// * `db` - Where to store the hash database.
    pub fn new(lower_limit: u64, roots: Vec<PathBuf>, db: PathBuf) -> Self {
        Self { lower_limit, roots, db }
    }

    /// Get the roots that this instance was initialized with.
    ///
    /// A root is a path where searching starts from.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Get the path to the database.
    pub fn db_path(&self) -> &Path {
        &self.db
    }
}

pub type Duplicates = HashMap<(u64, Hash), Vec<PathBuf>>;

/// Useful stats about a successful [find] operation.
pub struct Stats {
    /// A map from hashes to paths. If a hash points to multiple paths, then it
    /// means the files had the same hash, and are most likely duplicates of each
    /// other.
    pub duplicates: Duplicates,
    /// The number of files that have been hashed.
    pub total_files_processed: usize,
    /// The number of bytes that have been processed.
    pub total_bytes_processed: u64,
}

/// Finds and returns duplicated files on disk.
pub fn find(params: &Params) -> io::Result<Stats> {
    // TODO: what's a good minimum number?
    let num_threads = num_cpus::get().min(16);
    // give some leeway so that we don't hit the limit by accident
    let fds = rlimit::getrlimit(rlimit::Resource::NOFILE)?.0 as usize - 4 * num_threads;

    let (tx, rx) = mpsc::sync_channel(fds);
    let mut threads = VecDeque::with_capacity(num_threads);
    for _ in 0..num_threads {
        let (thread_tx, thread_rx) = mpsc::sync_channel(fds / num_threads);
        let db = params.db.clone();
        let tx = tx.clone();
        let handle = std::thread::spawn(move || hasher_task(db, thread_rx, tx));
        threads.push_back((handle, thread_tx));
    }
    let collector = std::thread::spawn(|| collect(rx));
    drop(tx);

    let mut total_files_processed = 0;
    let mut total_bytes_processed = 0;
    let mut next_worker = 0;
    for root in &params.roots {
        for entry in WalkDir::new(root) {
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

            let size = md.len();
            // TODO: other filters?
            if size < params.lower_limit {
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

            total_files_processed += 1;
            total_bytes_processed += size;
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
    Ok(Stats {
        duplicates: collector.join().expect("failed to join with collector"),
        total_files_processed,
        total_bytes_processed,
    })
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
    db: PathBuf,
    tasks: Receiver<(PathBuf, File, FileTime)>,
    tx: SyncSender<(PathBuf, io::Result<(u64, Hash)>)>,
) -> rusqlite::Result<()> {
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

fn collect(rx: Receiver<(PathBuf, io::Result<(u64, Hash)>)>) -> Duplicates {
    let mut entries: Duplicates = HashMap::new();
    while let Ok((path, res)) = rx.recv() {
        let (size, hash) = match res {
            Ok(h) => h,
            Err(e) => {
                eprintln!("failed to read from {}: {}", path.display(), e);
                continue;
            }
        };
        match entries.entry((size, hash)) {
            Occupied(mut v) => v.get_mut().push(path),
            Vacant(v) => {
                v.insert(vec![path]);
            }
        }
    }
    entries
}
