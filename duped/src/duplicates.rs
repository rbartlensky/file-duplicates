use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use blake3::Hash;

/// Metadata about a file that has been processed by [`crate::Deduper`].
#[derive(Clone, Debug)]
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

/// Files that share the same hash.
#[derive(Debug)]
pub struct FileEntries {
    files: Vec<FileEntry>,
    file_size: u64,
}

impl FileEntries {
    /// Create a new instance.
    pub(crate) fn new(files: Vec<FileEntry>) -> Self {
        let file_size = files.get(0).map(|e| e.size()).unwrap_or(0);

        Self { files, file_size }
    }

    pub(crate) fn push(&mut self, entry: FileEntry) {
        if self.files.is_empty() {
            self.file_size = entry.size();
            self.files.push(entry);
        } else {
            assert_eq!(self.file_size, entry.size());
            self.files.push(entry);
        }
    }

    pub(crate) fn has_duplicates(&self) -> bool {
        self.files.len() > 1
    }

    /// The file size shared by all entries.
    ///
    /// Since `FileEntries` stores all files that were hashed to the same value, each [`FileEntry`] is going to have the same size. This value is returned from this function.
    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    /// Return all [`FileEntry`]s stored by this instance.
    pub fn file_entries(&self) -> &[FileEntry] {
        &self.files
    }
}

/// A collection of duplicates.
#[derive(Debug, Default)]
pub struct DeduperResult {
    /// A list of file entries, grouped by their content's hash.
    hashes: HashMap<Hash, FileEntries>,
    /// Whether the user interrupted the find operations.
    is_partial: bool,
}

impl DeduperResult {
    /// Make this instance return true from `is_partial`.
    pub(crate) fn set_partial(&mut self) {
        self.is_partial = true;
    }

    /// Add a new entry into the duplicates map.
    pub(crate) fn add_entry(&mut self, hash: Hash, file: FileEntry) {
        self.hashes.entry(hash).or_insert_with(|| FileEntries::new(vec![])).push(file)
    }

    /// Get the collection of hashes and files that were gathered during [`crate::Deduper::find`].
    ///
    /// Each entry consists of a hash, and all the files that share the same hash. If an entry has only one path, that
    /// means it has no duplicates.
    pub fn hashes(&self) -> &HashMap<Hash, FileEntries> {
        &self.hashes
    }

    /// Return an interator of all duplicated file entries.
    pub fn duplicates(&self) -> impl Iterator<Item = (&Hash, &FileEntries)> {
        self.hashes.iter().filter(|(_, entries)| entries.has_duplicates())
    }

    /// Return `true` if the find operation was stopped prematurely and the results are only partial.
    ///
    /// This will be true, for example, if the deduper has files left to process, but [`DeduperStop::should_stop`]
    /// returned true.
    pub fn is_partial(&self) -> bool {
        self.is_partial
    }
}
