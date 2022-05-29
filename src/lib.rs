//! This library can be used to find duplicated files starting from a particular
//! root directory. To use it, please look over the following docs: [find],
//! [Params], and [Stats].
//!
//! # Examples
//!
//! ```no_run
//! use file_duplicates::{find, Params};
//!
//! let params = Params { lower_limit: 0, root: "./".into() };
//! let stats = find(&params).unwrap();
//! ```

use blake3::Hash;
use std::{
    collections::{
        hash_map::Entry::{Occupied, Vacant},
        HashMap, VecDeque,
    },
    fs::File,
    io::{self, BufRead, BufReader},
    path::PathBuf,
    sync::mpsc::{self, Receiver, SyncSender},
};
use walkdir::WalkDir;

/// Used to configure the [find] function.
pub struct Params {
    /// If the size of the file is under `lower_limit` bytes, it is not taken
    /// into account.
    pub lower_limit: u64,
    /// Where to start the search from.
    pub root: PathBuf,
}

pub type Duplicates = HashMap<Hash, Vec<(u64, PathBuf)>>;

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

    let (tx, rx) = mpsc::sync_channel(fds as usize);
    let mut threads = VecDeque::with_capacity(num_threads);
    for _ in 0..num_threads {
        let (thread_tx, thread_rx) = mpsc::sync_channel(fds / num_threads);
        let tx = tx.clone();
        let handle = std::thread::spawn(move || hasher_task(thread_rx, tx));
        threads.push_back((handle, thread_tx));
    }
    let collector = std::thread::spawn(|| collect(rx));
    drop(tx);

    let mut total_files_processed = 0;
    let mut total_bytes_processed = 0;
    let mut next_worker = 0;
    for entry in WalkDir::new(&params.root) {
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
        let size = match path.metadata() {
            Ok(md) => md.len(),
            Err(e) => {
                eprintln!("io error when reading metadata {}: {}", path.display(), e);
                continue;
            }
        };

        // TODO: other filters?
        if size < params.lower_limit {
            continue;
        }

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
                match tx.try_send((path, file)) {
                    Ok(()) => break 'outer,
                    Err(mpsc::TrySendError::Full((p, f))) => {
                        path = p;
                        file = f;
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        return Err(io::Error::new(
                            io::ErrorKind::Other,
                            "application didn't behave correctly",
                        ))
                    }
                }
            }
            std::thread::yield_now();
        }
    }
    // XXX: why doesn't rust "drop in place" rx if I use `_`?
    for (t, rx) in threads {
        drop(rx);
        t.join().expect("failed to join with thread");
    }
    Ok(Stats {
        duplicates: collector.join().expect("failed to join with collector"),
        total_files_processed,
        total_bytes_processed,
    })
}

fn hash_file(file: File) -> io::Result<(u64, Hash)> {
    // blake3 docs suggest a 16 KiB buffer for best performance
    let mut reader = BufReader::with_capacity(byte_unit::n_kib_bytes(16) as usize, file);
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
    tasks: Receiver<(PathBuf, File)>,
    tx: SyncSender<(PathBuf, io::Result<(u64, Hash)>)>,
) {
    while let Ok((path, file)) = tasks.recv() {
        if tx.send((path, hash_file(file))).is_err() {
            eprintln!("failed to send hash, quiting...");
            break;
        }
    }
}

fn collect(rx: Receiver<(PathBuf, io::Result<(u64, Hash)>)>) -> Duplicates {
    let mut entries: HashMap<Hash, Vec<(u64, PathBuf)>> = HashMap::new();
    while let Ok((path, res)) = rx.recv() {
        let (size, hash) = match res {
            Ok(h) => h,
            Err(e) => {
                eprintln!("failed to read from {}: {}", path.display(), e);
                continue;
            }
        };
        match entries.entry(hash) {
            Occupied(mut v) => v.get_mut().push((size, path)),
            Vacant(v) => {
                v.insert(vec![(size, path)]);
            }
        }
    }
    entries
}
