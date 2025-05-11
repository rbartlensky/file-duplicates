//! A collection of traits that can be used to configure the [`crate::Deduper`]'s behaviour, and also to subscribe to
//! particular events (such as a file being processed).
//!
//! See also: [`NoopStopper`], [`CotentLimit`], and [`NoopFindHook`].

use crate::duplicates::FileEntry;

use blake3::Hash;

use std::{fs, ops::ControlFlow, path::Path};

/// What to do with a file before the file deduper processes it.
pub enum FileAction {
    /// Include it in the analysis.
    Include,
    /// Exclude it from the analysis.
    Exclude,
}

/// The action to take after a file is selected by the deduper.
///
/// Callers can end the "main loop" of the deduper sooner, or choose to include/exclude the file.
pub type FilterAction = ControlFlow<(), FileAction>;

/// [`crate::Deduper`] calls [`Self::include_file`] for every file it encounters while recursing into the
/// configured roots.
pub trait DeduperFileFilter {
    /// Return whether the given file should be processed by [`crate::Deduper`].
    ///
    /// # Arguments
    ///
    /// * `path` - The path of the file.
    /// * `metadata` - The filesystem metadata of the file.
    ///
    /// # Notes
    ///
    /// The [`crate::Deduper`] will skip processing a file if it fails to read its metadata.
    ///
    /// By default, all files are included.
    fn handle_file(&mut self, _path: &Path, _metadata: &fs::Metadata) -> FilterAction {
        FilterAction::Continue(FileAction::Include)
    }
}

/// [`crate::Deduper`] calls [`Self::entry_processed`] for every file it hashed successfully.
pub trait DeduperFindHook: Send + Sync + 'static {
    /// Hook that is called when the [`crate::Deduper`] finished hashing a file.
    ///
    /// Users are encouraged to use this method to get updates on the progress of [`crate::Deduper::find`].
    ///
    /// The default implementation does nothing.
    fn entry_processed(&self, _hash: Hash, _entry: &FileEntry) {}
}

/// A [`DeduperFileClassifier`] that only allows files whose content is between a min and a max to be processed by a
/// [`crate::Deduper`].
#[derive(Debug)]
pub struct ContentLimit {
    /// If the size of the file is under `lower_limit` bytes, it is not taken into account.
    lower_limit: Option<u64>,
    /// If the size of the file is over `upper_limit` bytes, it is not taken into account.
    upper_limit: Option<u64>,
}

impl ContentLimit {
    /// Create an instance with no lower and upper limits.
    pub fn no_limit() -> Self {
        Self { lower_limit: None, upper_limit: None }
    }

    /// Recreate the instance with a new lower limit.
    pub fn with_lower_limit(mut self, lower_limit: u64) -> Self {
        self.lower_limit = Some(lower_limit);

        self
    }

    /// Recreate the instance with a new upper limit.
    pub fn with_upper_limit(mut self, upper_limit: u64) -> Self {
        self.upper_limit = Some(upper_limit);

        self
    }

    fn include_file_inner(&self, size: u64) -> FilterAction {
        if let Some(lower) = self.lower_limit {
            if size < lower {
                return FilterAction::Continue(FileAction::Exclude);
            }
        }

        if let Some(upper) = self.upper_limit {
            if size > upper {
                return FilterAction::Continue(FileAction::Exclude);
            }
        }

        FilterAction::Continue(FileAction::Include)
    }
}

impl DeduperFileFilter for ContentLimit {
    fn handle_file(&mut self, _: &Path, metadata: &fs::Metadata) -> FilterAction {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            let size = metadata.size();
            return self.include_file_inner(size);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;

            let size = metadata.file_size();
            return self.include_file_inner(size);
        }
    }
}

/// A [`DeduperFindHook`] that doesn't do anything.
pub struct NoopFindHook;

impl DeduperFindHook for NoopFindHook {}
