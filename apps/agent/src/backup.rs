//! `vessel-agent backup <file>` and `vessel-agent restore <file> <database>`.
//!
//! The passphrase comes from `VESSEL_BACKUP_PASSPHRASE` (never a command-line argument, which
//! would land in shell history and process listings). See `vessel_crypto::backup`.

use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter};
use std::path::Path;

use anyhow::{Context, bail};

fn passphrase() -> anyhow::Result<Vec<u8>> {
    let p = std::env::var("VESSEL_BACKUP_PASSPHRASE")
        .context("set VESSEL_BACKUP_PASSPHRASE (at least 16 characters, kept outside the backup)")?;
    if p.chars().count() < 16 {
        bail!("VESSEL_BACKUP_PASSPHRASE needs at least 16 characters");
    }
    Ok(p.into_bytes())
}

fn create_new(path: &Path) -> anyhow::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("{} already exists or cannot be created", path.display()))
}

/// Takes a consistent copy of the live database and writes it encrypted to `out`.
pub fn backup(database: &Path, out: &Path) -> anyhow::Result<()> {
    backup_with(&passphrase()?, database, out)
}

fn backup_with(pass: &[u8], database: &Path, out: &Path) -> anyhow::Result<()> {
    // Created first: an existing file is never touched, and only a file this run created is
    // removed on failure.
    let output = create_new(out)?;
    // The plain copy stays next to the database (same protection), only while encrypting.
    let plain = database.with_extension(format!("backup-{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&plain);
    let result = (|| {
        let conn = rusqlite::Connection::open(database).context("open the database")?;
        conn.execute("VACUUM INTO ?1", [plain.to_string_lossy().as_ref()])
            .context("copy the database")?;
        drop(conn);
        let mut input = BufReader::new(File::open(&plain)?);
        let mut output = BufWriter::new(output);
        vessel_crypto::backup::encrypt(pass, &mut input, &mut output)
            .map_err(|e| anyhow::anyhow!("encrypt: {e}"))
    })();
    let _ = std::fs::remove_file(&plain);
    if result.is_err() {
        let _ = std::fs::remove_file(out);
    }
    result
}

/// Decrypts `backup` into a new database file (never over an existing one).
pub fn restore(backup: &Path, database: &Path) -> anyhow::Result<()> {
    restore_with(&passphrase()?, backup, database)
}

fn restore_with(pass: &[u8], backup: &Path, database: &Path) -> anyhow::Result<()> {
    let mut input = BufReader::new(File::open(backup).context("open the backup")?);
    let mut output = BufWriter::new(create_new(database)?);
    if let Err(e) = vessel_crypto::backup::decrypt(pass, &mut input, &mut output) {
        drop(output);
        let _ = std::fs::remove_file(database);
        bail!("the backup cannot be restored: {e}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_backup_restores_the_same_database() {
        let dir = std::env::temp_dir().join(format!("vessel-backup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("vessel.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('kept');").unwrap();
        drop(conn);
        let pass = b"a long backup passphrase";
        let file = dir.join("backup.vsl");
        backup_with(pass, &db, &file).unwrap();
        // The backup is not a readable database.
        assert!(!std::fs::read(&file).unwrap().starts_with(b"SQLite format"));
        // Never over an existing file.
        assert!(backup_with(pass, &db, &file).is_err());
        assert!(restore_with(pass, &file, &db).is_err());

        let restored = dir.join("restored.db");
        assert!(restore_with(b"the wrong passphrase!!", &file, &restored).is_err());
        assert!(!restored.exists());
        restore_with(pass, &file, &restored).unwrap();
        let v: String = rusqlite::Connection::open(&restored)
            .unwrap()
            .query_row("SELECT v FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, "kept");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
