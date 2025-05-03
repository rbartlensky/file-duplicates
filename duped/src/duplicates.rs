use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use blake3::Hash;

/// Metadata about a file that has been processed by [`crate::Deduper`].
#[derive(Clone)]
pub struct FileEntry {
    path: PathBuf,
    size: u64,
}

impl FileEntry {
    /// Create a new instance.
    pub fn new(path: PathBuf, size: u64) -> Self {
        Self { path, size }
    }

    /// Get the path of the file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get the size of the file.
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// A collection of duplicates.
#[derive(Default)]
pub struct Duplicates {
    /// A list of file entries, grouped by their content's hash.
    hashes: HashMap<Hash, Vec<FileEntry>>,
}

impl Duplicates {
    /// Add a new entry into the duplicates map.
    pub(crate) fn add_entry(&mut self, hash: Hash, file: FileEntry) {
        self.hashes.entry(hash).or_default().push(file)
    }

    /// Get the collection of hashes and files that were gathered during [`crate::Deduper::find`].
    ///
    /// Each entry consists of a hash, and all the files that share the same hash. If an entry has only one path, that
    /// means it has no duplicates.
    pub fn hashes(&self) -> &HashMap<Hash, Vec<FileEntry>> {
        &self.hashes
    }
}
