//! Provides utilities to hash files in a progressive manner (i.e. in chunks, rather than entire files in one go).

use crate::file::FilePath;

use std::io::{self, Read, Seek};

/// A hasher that can be used to hash a file progressively.
pub struct ProgressiveHasher {
    /// Our hasher instance that might have some data in it already.
    hasher: blake3::Hasher,
    /// The file we are hashing chunk by chunk.
    file_path: FilePath,
    /// How much of a file we already hashed.
    len_hashed: u64,
}

// 16 KiBs
const MIN_TO_READ: u64 = 16 * 1024 * 1024;

impl ProgressiveHasher {
    /// Creates a new instance with a given [`FilePath`].
    ///
    /// # Arguments
    ///
    /// * `file_path` - The path of the file this instance will progressively hash.
    pub fn new(file_path: FilePath) -> Self {
        Self { hasher: Default::default(), file_path, len_hashed: 0 }
    }

    /// Gets the inner file path.
    pub fn file_path(&self) -> &FilePath {
        &self.file_path
    }

    /// Hashes the next 16KiBs of the file.
    ///
    /// Note, this method is going to open a _new_ file handle.
    pub fn update(&mut self) -> io::Result<()> {
        let leftover = self.file_path.metadata().len() - self.len_hashed;
        let bytes_to_take = leftover.min(MIN_TO_READ);

        let mut file = std::fs::File::open(self.file_path.path())?;

        file.seek(io::SeekFrom::Start(self.len_hashed))?;
        let reader = file.take(bytes_to_take);

        self.hasher.update_reader(reader)?;

        self.len_hashed += bytes_to_take;

        Ok(())
    }

    /// Returns whether the hasher finished hashing the entire input.
    pub fn current_hash(&self) -> (blake3::Hash, bool) {
        let hash = self.hasher.finalize();
        let done = self.len_hashed == self.file_path.metadata().len();

        (hash, done)
    }
}

/// A set of hashers.
#[derive(Default)]
pub(crate) struct HasherSet {
    inner: std::collections::HashMap<blake3::Hash, Vec<ProgressiveHasher>>,
}

impl HasherSet {
    /// Inserts the given hasher into the set.
    pub(crate) fn insert(&mut self, hasher: ProgressiveHasher) {
        self.inner.entry(hasher.current_hash().0).or_default().push(hasher);
    }

    /// Returns all hashers that still need some work.
    pub(crate) fn filter_unfinished_duplicates(
        self,
    ) -> (Vec<ProgressiveHasher>, Vec<ProgressiveHasher>) {
        let mut finished_hashers = vec![];
        let mut output_hashers = vec![];
        for (_, mut hashers) in self.inner {
            if hashers.len() == 1 {
                // safe to remove since the len of the vec is 1
                finished_hashers.push(hashers.remove(0));
            } else {
                output_hashers.extend(hashers);
            }
        }

        (finished_hashers, output_hashers)
    }
}
