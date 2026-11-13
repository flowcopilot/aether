//! Node settings in the configuration table. Each feature defines its own
//! settings next to its code; this module only reads and writes them.

use std::{
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
};

use aether_store::{NodeStore, StoreError};

/// A node setting in the configuration table, with the type of its value.
pub trait Setting {
    /// The configuration key.
    const KEY: &'static str;
    type Value;

    /// The value when the key is not stored.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] when the setting has no default.
    fn absent() -> Result<Self::Value, StoreError> {
        Err(StoreError::NotFound)
    }

    /// The value of the stored text.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] when the text is not a value.
    fn decode(stored: String) -> Result<Self::Value, StoreError>;

    /// The text to store for `value`; `None` removes the key.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] when the value has no text form.
    fn encode(value: &Self::Value) -> Result<Option<String>, StoreError>;
}

/// The stored value of the setting `S`.
///
/// # Errors
///
/// A [`StoreError`] when the store fails, or as [`Setting::absent`] and
/// [`Setting::decode`] return.
pub fn setting<S: Setting>(data_dir: &Path) -> Result<S::Value, StoreError> {
    match stored_text(data_dir, S::KEY) {
        Ok(stored) => S::decode(stored),
        Err(StoreError::NotFound) => S::absent(),
        Err(error) => Err(error),
    }
}

/// How many data directories keep an open reader.
const READERS: usize = 8;

/// The readers of the settings, oldest first. A setting read is one short
/// query, so one lock covers the lookup and the read.
static READERS_OPEN: Mutex<Vec<KeptStore>> = Mutex::new(Vec::new());

/// Which file a path names.
#[derive(Clone, Copy, Eq, PartialEq)]
struct FileId {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn file_id(path: &Path) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    Some(FileId { device: metadata.dev(), inode: metadata.ino() })
}

/// Without a file identity a reader cannot tell a replaced database, so
/// every use opens the store.
#[cfg(not(unix))]
fn file_id(_path: &Path) -> Option<FileId> {
    None
}

/// The node store of a directory, which stays open between uses, so that a
/// caller that reads often does not open the database and check its schema
/// each time.
///
/// Each use starts a new read transaction, so it sees what another
/// connection or process committed. Before a use it checks two things that
/// a connection that stays open would not notice: that the path still names
/// the file that was opened (an operator restored a backup or made the data
/// directory again), and that `user_version` is the one of this binary
/// (another binary migrated the file). If either differs, the store opens
/// again through [`NodeStore::open`], which migrates an older schema and
/// refuses a newer one, so a kept store fails closed as a new open does.
pub(crate) struct KeptStore {
    data_dir: PathBuf,
    create_dir: bool,
    /// The open store, with the file that it opened. Without a file identity
    /// the store is never reused.
    open: Option<(Option<FileId>, NodeStore)>,
}

impl KeptStore {
    /// A store of `data_dir` that opens on first use. With `create_dir` the
    /// first use creates a missing directory.
    pub(crate) const fn new(data_dir: PathBuf, create_dir: bool) -> Self {
        Self { data_dir, create_dir, open: None }
    }

    /// Drops the open store, so that the next use opens the file again.
    pub(crate) fn close(&mut self) {
        self.open = None;
    }

    fn database(&self) -> PathBuf {
        self.data_dir.join(aether_store::NODE_DATABASE)
    }

    /// Whether the open store is of the file that the path names now and has
    /// the schema of this binary.
    fn is_current(&self) -> bool {
        let Some((Some(open_file), store)) = &self.open else {
            return false;
        };
        file_id(&self.database()) == Some(*open_file) && store.schema_is_current()
    }

    /// The store for one use: the open one while it is current, else a new
    /// one. It runs nothing on the store, so a caller can run work that must
    /// not run twice.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] when the store cannot open, for example
    /// [`StoreError::Migration`] for a file that a newer binary migrated.
    pub(crate) fn current(&mut self) -> Result<&mut NodeStore, StoreError> {
        if !self.is_current() {
            self.open = None;
            if self.create_dir {
                std::fs::create_dir_all(&self.data_dir)?;
            }
            let store = NodeStore::in_dir(&self.data_dir)?;
            // The file that the store opened is the one that the path names
            // now, unless it changed in between; then the next use sees the
            // change.
            self.open = Some((file_id(&self.database()), store));
        }
        self.open.as_mut().map(|(_, store)| store).ok_or(StoreError::InvalidState)
    }

    /// The result of `read` on the current store.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] when the store cannot open, or as `read` returns. A
    /// SQL error on a store that was open before is tried once on a new
    /// store.
    pub(crate) fn read<R>(
        &mut self,
        read: impl Fn(&NodeStore) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        let was_open = self.is_current();
        match read(self.current()?) {
            Err(StoreError::Sql(_)) if was_open => {
                self.open = None;
                read(self.current()?)
            }
            answer => answer,
        }
    }
}

/// The stored text of `key`, read on an open store of `data_dir`.
fn stored_text(data_dir: &Path, key: &str) -> Result<String, StoreError> {
    let mut readers = READERS_OPEN.lock().unwrap_or_else(PoisonError::into_inner);
    let index = readers
        .iter()
        .position(|reader| reader.data_dir == data_dir)
        .unwrap_or_else(|| {
            if readers.len() >= READERS {
                readers.remove(0);
            }
            readers.push(KeptStore::new(data_dir.to_path_buf(), false));
            readers.len().saturating_sub(1)
        });
    let answer = readers
        .get_mut(index)
        .ok_or(StoreError::InvalidState)?
        .read(|store| store.configuration(key));
    drop(readers);
    answer
}

/// Keeps `value` for the setting `S`.
///
/// # Errors
///
/// A [`StoreError`] when the store fails or the value has no text form.
pub fn store_setting<S: Setting>(
    data_dir: &Path,
    value: &S::Value,
) -> Result<(), StoreError> {
    let store = NodeStore::in_dir(data_dir)?;
    match S::encode(value)? {
        Some(text) => store.put_configuration(S::KEY, &text),
        None => store.delete_configuration(S::KEY),
    }
}

#[cfg(test)]
mod tests {
    use aether_store::{NODE_DATABASE, NodeStore, StoreError};

    use super::{KeptStore, Setting, setting, store_setting};

    struct Colour;

    impl Setting for Colour {
        const KEY: &'static str = "test.colour";
        type Value = Option<String>;

        fn absent() -> Result<Self::Value, StoreError> {
            Ok(None)
        }

        fn decode(stored: String) -> Result<Self::Value, StoreError> {
            Ok(Some(stored))
        }

        fn encode(value: &Self::Value) -> Result<Option<String>, StoreError> {
            Ok(value.clone())
        }
    }

    fn colour(directory: &std::path::Path) -> Option<String> {
        setting::<Colour>(directory).expect("setting must read")
    }

    #[test]
    fn a_reader_that_stays_open_sees_each_later_write_and_delete() {
        let directory = tempfile::tempdir().expect("directory");
        assert_eq!(colour(directory.path()), None);
        store_setting::<Colour>(directory.path(), &Some("red".to_owned()))
            .expect("store");
        assert_eq!(colour(directory.path()).as_deref(), Some("red"));
        // Another connection writes, as another process or the admin UI does.
        NodeStore::in_dir(directory.path())
            .expect("store")
            .put_configuration(Colour::KEY, "blue")
            .expect("write");
        assert_eq!(colour(directory.path()).as_deref(), Some("blue"));
        store_setting::<Colour>(directory.path(), &None).expect("delete");
        assert_eq!(colour(directory.path()), None);
    }

    #[test]
    fn a_replaced_database_file_is_read_again() {
        let directory = tempfile::tempdir().expect("directory");
        store_setting::<Colour>(directory.path(), &Some("red".to_owned()))
            .expect("store");
        assert_eq!(colour(directory.path()).as_deref(), Some("red"));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(
                directory.path().join(format!("{NODE_DATABASE}{suffix}")),
            );
        }
        assert_eq!(colour(directory.path()), None);
        store_setting::<Colour>(directory.path(), &Some("green".to_owned()))
            .expect("store");
        assert_eq!(colour(directory.path()).as_deref(), Some("green"));
    }

    fn absent(kept: &mut KeptStore) -> Result<String, StoreError> {
        kept.read(|store| store.configuration("absent").or(Ok(String::new())))
    }

    #[test]
    fn a_kept_store_fails_closed_for_a_file_that_a_newer_binary_migrated() {
        let directory = tempfile::tempdir().expect("directory");
        let mut kept = KeptStore::new(directory.path().to_path_buf(), false);
        absent(&mut kept).expect("the first read opens the store");
        let database = directory.path().join(NODE_DATABASE);
        let other = rusqlite::Connection::open(&database).expect("database must open");
        let version: i64 = other
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version must read");
        // A newer binary migrated the file while this connection stayed open.
        other.pragma_update(None, "user_version", version + 1).expect("bump");
        assert!(matches!(absent(&mut kept), Err(StoreError::Migration(_))));
        assert!(kept.open.is_none(), "a store that failed the check is dropped");
        assert!(matches!(absent(&mut kept), Err(StoreError::Migration(_))));
        other.pragma_update(None, "user_version", version).expect("restore");
        assert_eq!(absent(&mut kept).as_deref(), Ok(""));
    }

    #[cfg(unix)]
    #[test]
    fn a_kept_store_opens_a_file_that_replaced_the_old_one() {
        let directory = tempfile::tempdir().expect("directory");
        let mut kept = KeptStore::new(directory.path().to_path_buf(), false);
        kept.current()
            .expect("open")
            .put_configuration("test.k", "old")
            .expect("write");
        let first = kept.open.as_ref().and_then(|(file, _)| *file);
        // An operator restores a backup: the path names another file.
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(
                directory.path().join(format!("{NODE_DATABASE}{suffix}")),
            );
        }
        NodeStore::in_dir(directory.path())
            .expect("store")
            .put_configuration("test.k", "restored")
            .expect("write");
        let read =
            |kept: &mut KeptStore| kept.read(|store| store.configuration("test.k"));
        assert_eq!(read(&mut kept).as_deref(), Ok("restored"));
        let second = kept.open.as_ref().and_then(|(file, _)| *file);
        assert!(first.is_some() && second.is_some() && first != second);
    }

    #[test]
    fn a_kept_store_opens_once_for_many_reads() {
        let directory = tempfile::tempdir().expect("directory");
        let mut kept = KeptStore::new(directory.path().join("node"), true);
        let opened = std::cell::Cell::new(0_u32);
        for _ in 0..3 {
            kept.read(|store| {
                opened.set(opened.get().saturating_add(1));
                store.configuration("absent").or(Ok(String::new()))
            })
            .expect("read");
        }
        assert_eq!(opened.get(), 3);
        assert!(kept.open.is_some(), "the store stays open after a read");
        let first = kept.open.as_ref().map(|(file, _)| *file);
        kept.read(|store| store.configuration("absent").or(Ok(String::new())))
            .expect("read");
        assert!(kept.open.as_ref().map(|(file, _)| *file) == first);
    }
}
