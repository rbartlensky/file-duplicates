use rusqlite::{ffi, params, Connection, Error, Result};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

#[derive(Debug, Eq, PartialEq)]
pub struct Entry<'p> {
    pub path: &'p Path,
    pub mtime: i64,
    pub hash: blake3::Hash,
    pub size: u64,
}

/// A sqlite database which stores [Entry]s.
pub struct HashDb {
    conn: Connection,
}

impl HashDb {
    pub fn try_new(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute(
            "
CREATE TABLE IF NOT EXISTS path (
    path  BLOB PRIMARY KEY,
    mtime INTEGER NOT NULL,
    hash  BLOB NOT NULL,
    size  INTEGER NOT NULL
)",
            [],
        )?;
        Ok(Self { conn })
    }

    /// Inserts or updates an entry based on `entry`.
    pub fn insert(&self, entry: &Entry) -> Result<usize> {
        self.conn.execute(
            "
INSERT INTO path (path, mtime, hash, size)
VALUES (?1, ?2, ?3, ?4)
ON CONFLICT (path)
DO UPDATE SET mtime = excluded.mtime, hash = excluded.hash, size = excluded.size
",
            params![
                entry.path.as_os_str().as_bytes(),
                entry.mtime,
                entry.hash.as_bytes(),
                entry.size
            ],
        )
    }

    /// Removes the `path` entry.
    pub fn remove(&self, path: &Path) -> Result<usize> {
        self.conn
            .execute("DELETE FROM path WHERE path.path = ?1", params![path.as_os_str().as_bytes()])
    }

    /// Gets the first (and only) entry for `path`.
    pub fn select<'p>(&self, path: &'p Path) -> Result<Option<Entry<'p>>> {
        let mut stmt = self.conn.prepare("SELECT mtime, hash, size FROM path WHERE path = ?1")?;
        let mut iter = stmt.query_map([path.as_os_str().as_bytes()], |row| {
            let mut hash = [0; 32];
            let bytes = row.get_ref(1)?.as_blob()?;
            assert_eq!(bytes.len(), 32);
            hash.copy_from_slice(&bytes[..32]);
            Ok(Entry {
                path,
                mtime: row.get(0)?,
                hash: blake3::Hash::from(hash),
                size: row.get(2)?,
            })
        })?;
        iter.next().transpose()
    }
}

pub fn retry_on_busy<F, T>(mut f: F) -> Result<T>
where
    F: FnMut() -> Result<T>,
{
    loop {
        match f() {
            // we want to retry when database is busy
            Err(Error::SqliteFailure(
                ffi::Error { code: ffi::ErrorCode::DatabaseBusy, extended_code: 5 },
                _,
            )) => std::thread::yield_now(),
            e => return e,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn insert_and_remove() {
        let tempfile = tempfile::NamedTempFile::new().unwrap();
        let db = HashDb::try_new(tempfile.path()).unwrap();
        let path = PathBuf::from("/home/foo");
        let mut entry =
            Entry { path: &path, mtime: 2, hash: blake3::Hash::from([0; 32]), size: 10 };

        // insert our entry
        assert_eq!(db.insert(&entry).unwrap(), 1);
        assert_eq!(db.select(&path).unwrap().as_ref(), Some(&entry));

        // modify mtime so that we update the entry
        entry.mtime = 3;
        assert_eq!(db.insert(&entry).unwrap(), 1);
        // mtime was correctly updated
        assert_eq!(db.select(&path).unwrap().as_ref(), Some(&entry));

        // now the path should be gone
        assert_eq!(db.remove(&path).unwrap(), 1);
        assert_eq!(db.select(&path).unwrap(), None);
    }
}
