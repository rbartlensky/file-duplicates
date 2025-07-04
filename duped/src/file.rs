use std::{
    fs::Metadata,
    path::{Path, PathBuf},
};

/// A path and its metadata.
pub struct FilePath {
    path: PathBuf,
    metadata: Metadata,
}

impl FilePath {
    /// Creates a new instance by reading `path`'s metadata.
    pub fn try_new(path: PathBuf) -> std::io::Result<Self> {
        let metadata = path.metadata()?;
        Ok(Self { path, metadata })
    }

    /// Gets the path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Gets the metadata of a particular file.
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}
