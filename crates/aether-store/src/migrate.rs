use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, MutexGuard, PoisonError},
};

use rusqlite::{Connection, MAIN_DB};
use rusqlite_migration::{M, Migrations};

use crate::{StoreError, now};

pub const BACKUP_RETENTION: usize = 3;

/// The database files that this process has migrated and checked.
static MIGRATED: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);

/// Opens `path` in WAL mode. The first open of a path in this process makes
/// a backup if a migration is pending, migrates, and checks the foreign
/// keys. A later open only checks that `user_version` is the latest. If it
/// is not (another binary or a swapped file), the open does the full check
/// again, which fails for a newer schema.
pub fn open_connection(
    path: &Path,
    migrations: &[&str],
) -> Result<Connection, StoreError> {
    let mut connection = Connection::open(path)?;
    connection.busy_timeout(std::time::Duration::from_secs(1))?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update_and_check(None, "journal_mode", "WAL", |row| {
        row.get::<_, String>(0)
    })?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    let key = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let checked = migrated().contains(&key);
    if checked && schema_is_latest(&connection, migrations)? {
        return Ok(connection);
    }
    // Hold the lock, so that one thread migrates a path at a time.
    let mut migrated = migrated();
    if migration_pending(&connection, migrations)? {
        backup_connection(&connection, path)?;
    }
    migrate(&mut connection, migrations)?;
    migrated.insert(key);
    drop(migrated);
    Ok(connection)
}

fn migrated() -> MutexGuard<'static, HashSet<PathBuf>> {
    MIGRATED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether the database at `connection` has every one of `migrations`.
fn schema_is_latest(
    connection: &Connection,
    migrations: &[&str],
) -> Result<bool, StoreError> {
    let user_version: i64 =
        connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    Ok(usize::try_from(user_version).is_ok_and(|version| version == migrations.len()))
}

pub fn backup_connection(
    connection: &Connection,
    path: &Path,
) -> Result<PathBuf, StoreError> {
    let backup = next_backup_path(path);
    connection.backup(MAIN_DB, &backup, None)?;
    retain_backups(path)?;
    Ok(backup)
}

fn next_backup_path(path: &Path) -> PathBuf {
    let prefix = format!("{}.backup.{}", path.display(), now());
    let mut index: u32 = 0;
    loop {
        let candidate = if index == 0 {
            PathBuf::from(&prefix)
        } else {
            PathBuf::from(format!("{prefix}.{index}"))
        };
        if !candidate.exists() {
            return candidate;
        }
        index = index.saturating_add(1);
    }
}

fn retain_backups(path: &Path) -> Result<(), StoreError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().ok_or(StoreError::InvalidState)?;
    let prefix = format!("{}{}", name.to_string_lossy(), ".backup.");
    let mut backups = fs::read_dir(parent)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|entry| {
            entry
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .collect::<Vec<_>>();
    backups.sort();
    while backups.len() > BACKUP_RETENTION {
        let backup = backups.remove(0);
        fs::remove_file(backup)?;
    }
    Ok(())
}

pub fn migrate(
    connection: &mut Connection,
    migrations: &[&str],
) -> Result<(), StoreError> {
    let applied = legacy_migration_version(connection)?.unwrap_or_default();
    let user_version: i64 =
        connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if user_version == 0 && applied != 0 {
        connection.pragma_update(None, "user_version", applied)?;
    }
    let migrations =
        Migrations::new(migrations.iter().map(|migration| M::up(migration)).collect());
    migrations.to_latest(connection)?;
    let foreign_key_error: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)",
        [],
        |row| row.get(0),
    )?;
    if foreign_key_error {
        return Err(StoreError::InvalidState);
    }
    Ok(())
}

fn migration_pending(
    connection: &Connection,
    migrations: &[&str],
) -> Result<bool, StoreError> {
    let user_version: i64 =
        connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if user_version != 0 {
        return Ok(version_is_behind(user_version, migrations));
    }
    let Some(applied) = legacy_migration_version(connection)? else {
        return Ok(false);
    };
    Ok(version_is_behind(applied, migrations))
}

/// Whether a database at schema `version` still has `migrations` to apply.
/// A negative version is behind; one past every migration is not.
fn version_is_behind(version: i64, migrations: &[&str]) -> bool {
    usize::try_from(version).map_or(true, |version| version < migrations.len())
}

fn legacy_migration_version(
    connection: &Connection,
) -> Result<Option<i64>, StoreError> {
    let has_table: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(None);
    }
    connection
        .query_row(
            "SELECT COALESCE(MAX(version),0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .map(Some)
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use rusqlite::Connection;

    use super::*;
    use crate::{test_support::*, *};

    fn backup(path: &Path) -> Result<PathBuf, StoreError> {
        backup_connection(&Connection::open(path)?, path)
    }

    #[test]
    fn a_0_2_8_database_opens_with_every_migration_and_an_older_binary_refuses_it() {
        // Release 0.2.8 had the node migrations 001 to 011.
        let database = path("release-0-2-8");
        let mut connection = Connection::open(&database).expect("database must open");
        migrate(&mut connection, &NODE_MIGRATIONS[..11]).expect("the 0.2.8 schema");
        connection
            .execute(
                "INSERT INTO access_group_members(group_name,agent_id,endpoint_id) VALUES('friends','ana','endpoint-ana')",
                [],
            )
            .expect("member must insert");
        connection
            .execute(
                "INSERT INTO configuration(key,value) VALUES('caller.contact.ana','{}')",
                [],
            )
            .expect("contact must insert");
        drop(connection);

        let store = NodeStore::open(&database).expect("every migration must apply");
        let version: i64 = store
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version must read");
        assert_eq!(usize::try_from(version).ok(), Some(NODE_MIGRATIONS.len()));
        assert_eq!(store.configuration("caller.contact.ana").as_deref(), Ok("{}"));
        let groups = store.access_groups().expect("groups must read");
        let names: Vec<&str> = groups.iter().map(|group| group.name.as_str()).collect();
        assert_eq!(names, ["friends", "requests"]);
        assert_eq!(groups[0].members.len(), 1);
        assert_eq!(groups[0].members[0].agent_id, "ana");
        assert!(groups[1].members.is_empty());
        assert_eq!(backup_count(&database), 1);
        drop(store);

        // The code of 0.2.8 knows 11 migrations and refuses the newer schema.
        let mut connection = Connection::open(&database).expect("database must open");
        assert!(matches!(
            migrate(&mut connection, &NODE_MIGRATIONS[..11]),
            Err(StoreError::Migration(_))
        ));
    }

    #[test]
    fn a_later_open_in_the_process_still_checks_the_schema_version() {
        let database = path("later-open");
        drop(NodeStore::open(&database).expect("first open must migrate"));
        let latest = i64::try_from(NODE_MIGRATIONS.len()).expect("count fits");
        let connection = Connection::open(&database).expect("database must open");
        connection
            .pragma_update(None, "user_version", latest + 1)
            .expect("version must write");
        assert!(matches!(NodeStore::open(&database), Err(StoreError::Migration(_))));
        connection
            .pragma_update(None, "user_version", latest)
            .expect("version must write");
        drop(connection);
        let store = NodeStore::open(&database).expect("current schema must open");
        let mode: String = store
            .connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("journal mode must read");
        assert_eq!(mode, "wal");
    }

    #[test]
    fn a_backup_keeps_the_store_as_it_was_when_taken() {
        let (mut store, database) = store();
        ControlStore::open(path("control")).expect("control must open");
        store.put_configuration("owner", "operator").expect("configuration must store");
        store
            .admit(&input("one", "a"), QueueLimits { global: 2, per_peer: 2 })
            .expect("task must admit");
        let saved = backup(&database).expect("backup must work");
        store.cancel("one").expect("task must cancel");
        drop(store);
        let saved_store = NodeStore::open(&saved).expect("backup must open");
        assert_eq!(saved_store.configuration("owner"), Ok("operator".to_owned()));
        assert_eq!(saved_store.state("one"), Ok(TaskState::Queued));
        assert_eq!(
            saved_store
                .events("one")
                .expect("events must load")
                .into_iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1]
        );
        drop(saved_store);
        fs::remove_file(database).expect("database must remove");
        fs::remove_file(saved).expect("backup must remove");
    }

    #[test]
    fn migration_six_restores_task_foreign_keys_and_constraints() {
        let database = path("migration-six");
        let mut connection = Connection::open(&database).expect("database must open");
        migrate(&mut connection, &NODE_MIGRATIONS[..5])
            .expect("old migrations must apply");
        connection
            .execute("INSERT INTO scopes(id,kind) VALUES('scope','direct')", [])
            .expect("scope must insert");
        connection
            .execute(
                "INSERT INTO tasks(id,request_id,idempotency_key,caller_agent_id,caller_endpoint_id,request_fingerprint,scope_id,capability_id,workspace_id,access_mode,state,created_at) VALUES('task','request','key','agent','endpoint','fingerprint','scope','capability','workspace','read_only','queued',1)",
                [],
            )
            .expect("task must insert");
        connection
            .execute(
                "INSERT INTO task_events(task_id,sequence,event_type,data,created_at) VALUES('task',1,'queued','{}',1)",
                [],
            )
            .expect("event must insert");
        connection
            .execute(
                "INSERT INTO execution_leases(task_id,owner,execution_id,expires_at) VALUES('task','owner','execution',2)",
                [],
            )
            .expect("lease must insert");
        drop(connection);
        let store = NodeStore::open(&database).expect("repair migration must apply");
        assert_eq!(store.events("task").expect("events must load").len(), 1);
        let foreign_key_error: bool = store
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)",
                [],
                |row| row.get(0),
            )
            .expect("foreign key check must run");
        assert!(!foreign_key_error);
        assert!(store
            .connection
            .execute(
                "INSERT INTO task_events(task_id,sequence,event_type,data,created_at) VALUES('missing',1,'queued','{}',1)",
                [],
            )
            .is_err());
        assert!(store
            .connection
            .execute(
                "INSERT INTO execution_leases(task_id,owner,execution_id,expires_at) VALUES('missing','owner','execution',2)",
                [],
            )
            .is_err());
        assert!(store
            .connection
            .execute(
                "INSERT INTO task_events(task_id,sequence,event_type,data,created_at) VALUES('task',2,'','{}',1)",
                [],
            )
            .is_err());
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn legacy_migration_ledger_converts_once_to_user_version() {
        let database = path("legacy-migration-ledger");
        let connection = Connection::open(&database).expect("database must open");
        for migration in &NODE_MIGRATIONS[..5] {
            connection.execute_batch(migration).expect("legacy migration must apply");
        }
        connection
            .execute_batch(
                "CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY NOT NULL); INSERT INTO schema_migrations(version) VALUES(5);",
            )
            .expect("legacy ledger must store");
        drop(connection);
        let store = NodeStore::open(&database).expect("legacy database must convert");
        let user_version: i64 = store
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("user version must load");
        assert_eq!(usize::try_from(user_version).ok(), Some(NODE_MIGRATIONS.len()));
        drop(store);
        NodeStore::open(&database).expect("converted database must reopen");
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn opening_a_current_database_does_not_create_a_backup() {
        let database = path("current");
        let store = NodeStore::open(&database).expect("store must open");
        drop(store);
        NodeStore::open(&database).expect("current store must open");
        assert_eq!(backup_count(&database), 0);
        fs::remove_file(database).expect("database must remove");
    }

    #[test]
    fn online_backups_preserve_data_and_apply_retention() {
        let (store, database) = store();
        store.put_configuration("owner", "operator").expect("configuration must store");
        let mut backups = Vec::new();
        for _ in 0..=BACKUP_RETENTION {
            backups.push(backup(&database).expect("backup must work"));
        }
        assert_eq!(
            backups.iter().filter(|path| path.exists()).count(),
            BACKUP_RETENTION
        );
        let latest = backups.last().expect("backup must exist");
        let restored = NodeStore::open(latest).expect("backup must open");
        assert_eq!(restored.configuration("owner"), Ok("operator".to_owned()));
        drop(restored);
        drop(store);
        fs::remove_file(database).expect("database must remove");
        for backup in backups.into_iter().filter(|path| path.exists()) {
            fs::remove_file(backup).expect("backup must remove");
        }
    }

    #[test]
    fn constraints_cancellation_and_failed_migrations_keep_the_database_usable() {
        let (mut store, database) = store();
        let mut missing_scope = input("missing", "a");
        missing_scope.request.scope_id = aether_core::ScopeId::new("missing");
        assert!(
            store
                .admit(&missing_scope, QueueLimits { global: 1, per_peer: 1 })
                .is_err()
        );
        store
            .admit(&input("one", "a"), QueueLimits { global: 1, per_peer: 1 })
            .expect("task must admit");
        store.cancel("one").expect("task must cancel");
        assert_eq!(store.state("one"), Ok(TaskState::Cancelled));
        let migrations =
            NODE_MIGRATIONS.iter().copied().chain(["INVALID SQL"]).collect::<Vec<_>>();
        assert!(migrate(&mut store.connection, &migrations).is_err());
        store
            .put_configuration("after_failure", "usable")
            .expect("database must remain usable");
        fs::remove_file(database).expect("database must remove");
    }
}
