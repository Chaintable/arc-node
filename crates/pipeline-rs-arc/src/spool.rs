use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, MutexGuard,
    },
};

use alloy_primitives::B256;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{BlockRef, CaptureSink, CapturedArcBlock, ObjectKind, ObjectNamespace, SpoolError};

/// Bind a spool to one object namespace and one source database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpoolIdentity {
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub version: String,
    pub source_id: String,
    pub blockfile_bucket: String,
    pub nodex_bucket: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpoolBlock {
    pub block: BlockRef,
    pub state_root: B256,
    pub parent_state_root: B256,
    eligible: bool,
    required: u8,
    acknowledged: u8,
}

impl SpoolBlock {
    pub fn upload_eligible(&self) -> bool {
        self.eligible
    }

    pub fn remote_ready(&self) -> bool {
        self.eligible && self.acknowledged == self.required
    }

    pub fn missing_objects(&self) -> impl Iterator<Item = ObjectKind> + '_ {
        [
            ObjectKind::BlockFile,
            ObjectKind::Validation,
            ObjectKind::Header,
            ObjectKind::StateDiff,
        ]
        .into_iter()
        .filter(|kind| (self.required & !self.acknowledged) & kind.bit() != 0)
    }
}

/// Single-process spool. The file lock also protects orphan cleanup at startup.
pub struct Spool {
    identity: SpoolIdentity,
    raw_dir: PathBuf,
    index: Mutex<Connection>,
    file_quota: u64,
    raw_bytes: AtomicU64,
    _lock: File,
}

impl Spool {
    pub fn open(
        root: &Path,
        identity: &SpoolIdentity,
        file_quota: u64,
    ) -> Result<Self, SpoolError> {
        fs::create_dir_all(root)?;
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(root.join("writer.lock"))?;
        lock.try_lock().map_err(io::Error::from)?;
        let raw_dir = root.join("raw");
        fs::create_dir_all(&raw_dir)?;

        let mut index = Connection::open(root.join("index.sqlite"))?;
        index.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS identity (
                 singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                 schema_version INTEGER NOT NULL,
                 data TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS captures (
                 hash TEXT PRIMARY KEY,
                 height INTEGER NOT NULL,
                 block TEXT NOT NULL,
                 fingerprint BLOB NOT NULL,
                 raw_bytes INTEGER NOT NULL CHECK(raw_bytes >= 0),
                 raw_sha256 BLOB NOT NULL
             );
             CREATE INDEX IF NOT EXISTS capture_height ON captures(height);",
        )?;
        if let Some((version, data)) = index
            .query_row("SELECT schema_version, data FROM identity", [], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
            })
            .optional()?
        {
            if serde_json::from_str::<SpoolIdentity>(&data)? != *identity {
                return Err(SpoolError::WrongIdentity);
            }
            match version {
                1 => {
                    let tx = index.transaction()?;
                    tx.execute_batch(
                        "DROP INDEX IF EXISTS historical_capture_height;
                         DROP INDEX IF EXISTS pending_uploads;
                         UPDATE captures SET block = json_remove(
                             json_set(block, '$.eligible', json(CASE WHEN
                                 json_extract(block, '$.live_accepted')
                                 OR json_extract(block, '$.history_confirmed')
                             THEN 'true' ELSE 'false' END)),
                             '$.live_accepted', '$.history_confirmed', '$.historical_capture');
                         UPDATE identity SET schema_version = 2;",
                    )?;
                    tx.commit()?;
                }
                2 => {}
                _ => return Err(SpoolError::WrongIdentity),
            }
        } else {
            index.execute(
                "INSERT INTO identity VALUES (1, 2, ?1)",
                [serde_json::to_string(identity)?],
            )?;
        }
        #[cfg(feature = "delivery")]
        index.execute_batch(
            "CREATE INDEX IF NOT EXISTS pending_uploads ON captures(height, hash)
             WHERE json_extract(block, '$.eligible')
             AND json_extract(block, '$.acknowledged') != json_extract(block, '$.required');",
        )?;
        File::open(root)?.sync_all()?;

        let spool = Self {
            identity: identity.clone(),
            raw_dir,
            index: Mutex::new(index),
            file_quota,
            raw_bytes: AtomicU64::new(0),
            _lock: lock,
        };
        spool.recover_files()?;
        Ok(spool)
    }

    fn index(&self) -> MutexGuard<'_, Connection> {
        self.index.lock().expect("Writer spool index lock poisoned")
    }

    fn path(&self, hash: B256) -> PathBuf {
        self.raw_dir.join(format!("{hash:#x}.json"))
    }

    fn recover_files(&self) -> Result<(), SpoolError> {
        let index = self.index();
        for entry in fs::read_dir(&self.raw_dir)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(".capture-") {
                fs::remove_file(path)?;
            } else if let Some(hash) = name
                .strip_suffix(".json")
                .and_then(|value| value.parse::<B256>().ok())
            {
                if load_entry(&index, hash)?.is_none_or(|entry| entry.block.remote_ready()) {
                    fs::remove_file(path)?;
                }
            }
        }
        File::open(&self.raw_dir)?.sync_all()?;
        index.execute(
            "UPDATE captures SET raw_bytes = 0 WHERE raw_bytes != 0 AND
             json_extract(block, '$.acknowledged') = json_extract(block, '$.required')",
            [],
        )?;
        let bytes = index.query_row(
            "SELECT COALESCE(SUM(raw_bytes), 0) FROM captures",
            [],
            |row| row.get(0),
        )?;
        self.raw_bytes.store(bytes, Ordering::Relaxed);
        Ok(())
    }

    pub fn namespace(&self) -> ObjectNamespace<'_> {
        ObjectNamespace {
            chain_id: self.identity.chain_id,
            version: &self.identity.version,
            blockfile_bucket: &self.identity.blockfile_bucket,
            nodex_bucket: &self.identity.nodex_bucket,
        }
    }

    pub fn block(&self, hash: B256) -> Result<Option<SpoolBlock>, SpoolError> {
        Ok(load_entry(&self.index(), hash)?.map(|entry| entry.block))
    }

    #[cfg(feature = "delivery")]
    pub(crate) fn pending_uploads(&self, limit: u32) -> Result<Vec<B256>, SpoolError> {
        let index = self.index();
        let mut stmt = index.prepare_cached(
            "SELECT block FROM captures
             WHERE json_extract(block, '$.eligible')
               AND json_extract(block, '$.acknowledged') != json_extract(block, '$.required')
             ORDER BY height, hash LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| {
            let block: SpoolBlock = serde_json::from_str(&row?)?;
            Ok(block.block.hash)
        })
        .collect()
    }

    #[cfg(feature = "delivery")]
    pub(crate) fn publication_initialized(
        &self,
        topic: &str,
        partition: i32,
    ) -> Result<bool, SpoolError> {
        let index = self.index();
        index.execute_batch(
            "CREATE TABLE IF NOT EXISTS publication (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                topic TEXT NOT NULL,
                partition INTEGER NOT NULL,
                initialized INTEGER NOT NULL
             )",
        )?;
        index.execute(
            "INSERT OR IGNORE INTO publication VALUES (1, ?1, ?2, 0)",
            params![topic, partition],
        )?;
        let (stored_topic, stored_partition, initialized) = index.query_row(
            "SELECT topic, partition, initialized FROM publication WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            },
        )?;
        if stored_topic != topic || stored_partition != partition {
            return Err(SpoolError::WrongIdentity);
        }
        Ok(initialized)
    }

    #[cfg(feature = "delivery")]
    pub(crate) fn mark_publication_initialized(&self) -> Result<(), SpoolError> {
        self.index().execute(
            "UPDATE publication SET initialized = 1 WHERE singleton = 1",
            [],
        )?;
        Ok(())
    }

    /// Loads an immutable capture for an upload attempt.
    pub fn raw(&self, hash: B256) -> Result<CapturedArcBlock, SpoolError> {
        let (entry, file) = {
            let index = self.index();
            let entry = load_entry(&index, hash)?.ok_or(SpoolError::Corrupt(hash))?;
            let file = File::open(self.path(hash))?;
            (entry, file)
        };
        if file.metadata()?.len() != entry.raw_bytes {
            return Err(SpoolError::Corrupt(hash));
        }
        let mut reader = BufReader::new(file);
        let mut hasher = Sha256::new();
        io::copy(&mut reader, &mut hasher)?;
        if hasher.finalize().as_slice() != entry.raw_sha256 {
            return Err(SpoolError::Corrupt(hash));
        }
        use std::io::Seek;
        reader.rewind()?;
        Ok(serde_json::from_reader(reader)?)
    }

    /// Call only after the specified object has been confirmed remotely.
    pub fn acknowledge(&self, hash: B256, kind: ObjectKind) -> Result<(), SpoolError> {
        let index = self.index();
        let mut entry = load_entry(&index, hash)?.ok_or(SpoolError::Corrupt(hash))?;
        if entry.block.required & kind.bit() == 0 {
            return Err(SpoolError::Conflict(hash));
        }
        entry.block.acknowledged |= kind.bit();
        // SQLite durability precedes removal of the only local copy.
        update_block(&index, &entry.block)?;
        if entry.block.remote_ready() {
            match fs::remove_file(self.path(hash)) {
                Ok(()) => File::open(&self.raw_dir)?.sync_all()?,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
            clear_raw_size(&index, hash)?;
            self.raw_bytes.fetch_sub(entry.raw_bytes, Ordering::Relaxed);
        }
        Ok(())
    }
}

impl CaptureSink for Spool {
    fn seal(&self, block: CapturedArcBlock) -> Result<(), SpoolError> {
        block.validate_state_diff()?;
        let hash = block.block.hash;
        let fingerprint = content_fingerprint(&block)?;
        let index = self.index();
        if let Some(mut entry) = load_entry(&index, hash)? {
            if entry.fingerprint != fingerprint {
                return Err(SpoolError::Conflict(hash));
            }
            entry.block.eligible = true;
            if !entry.block.remote_ready() && !self.path(hash).is_file() {
                return Err(SpoolError::Corrupt(hash));
            }
            return update_block(&index, &entry.block);
        }

        let bytes = self.raw_bytes.load(Ordering::Relaxed);
        let mut temporary = tempfile::Builder::new()
            .prefix(".capture-")
            .tempfile_in(&self.raw_dir)?;
        let (size, raw_sha) = {
            let mut writer = QuotaWriter {
                sink: BufWriter::new(temporary.as_file_mut()),
                sha: Sha256::new(),
                written: 0,
                limit: self.file_quota.saturating_sub(bytes),
            };
            serde_json::to_writer(&mut writer, &block)?;
            writer.flush()?;
            (writer.written, writer.sha.finalize())
        };
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.path(hash))
            .map_err(|err| err.error)?;
        File::open(&self.raw_dir)?.sync_all()?;

        let entry = SpoolBlock {
            block: block.block,
            state_root: block.state_root,
            parent_state_root: block.parent_state_root,
            eligible: true,
            required: if block.state_root == block.parent_state_root {
                7
            } else {
                15
            },
            acknowledged: 0,
        };
        index.execute(
            "INSERT INTO captures VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                format!("{hash:#x}"),
                block.block.block_number,
                serde_json::to_string(&entry)?,
                fingerprint,
                size,
                raw_sha.as_slice()
            ],
        )?;
        self.raw_bytes.fetch_add(size, Ordering::Relaxed);
        Ok(())
    }
}

fn clear_raw_size(index: &Connection, hash: B256) -> Result<(), SpoolError> {
    index.execute(
        "UPDATE captures SET raw_bytes = 0 WHERE hash = ?1",
        [format!("{hash:#x}")],
    )?;
    Ok(())
}

fn update_block(index: &Connection, block: &SpoolBlock) -> Result<(), SpoolError> {
    index.execute(
        "UPDATE captures SET block = ?2 WHERE hash = ?1",
        params![
            format!("{:#x}", block.block.hash),
            serde_json::to_string(block)?
        ],
    )?;
    Ok(())
}

struct Entry {
    block: SpoolBlock,
    fingerprint: Vec<u8>,
    raw_bytes: u64,
    raw_sha256: Vec<u8>,
}

fn load_entry(index: &Connection, hash: B256) -> Result<Option<Entry>, SpoolError> {
    let record = index
        .query_row(
            "SELECT block, fingerprint, raw_bytes, raw_sha256 FROM captures WHERE hash = ?1",
            [format!("{hash:#x}")],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                ))
            },
        )
        .optional()?;
    record
        .map(|(data, fingerprint, raw_bytes, raw_sha256)| {
            Ok(Entry {
                block: serde_json::from_str(&data)?,
                fingerprint,
                raw_bytes,
                raw_sha256,
            })
        })
        .transpose()
}

fn content_fingerprint(block: &CapturedArcBlock) -> Result<Vec<u8>, SpoolError> {
    let mut file: serde_json::Value = serde_json::from_str(block.block_file.get())?;
    if let Some(metadata) = file
        .get_mut("block")
        .and_then(serde_json::Value::as_object_mut)
    {
        metadata.remove("process_start_timestamp");
    }
    let header: serde_json::Value = serde_json::from_str(block.header.get())?;
    let mut hash = Sha256::new();
    serde_json::to_writer(
        &mut hash,
        &(
            &block.block,
            &block.state_root,
            &block.parent_state_root,
            file,
            header,
            &block.validation,
            &block.state_diff_rlp,
        ),
    )?;
    Ok(hash.finalize().to_vec())
}

struct QuotaWriter<W> {
    sink: W,
    sha: Sha256,
    written: u64,
    limit: u64,
}

impl<W: Write> Write for QuotaWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.limit.saturating_sub(self.written) {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "Writer raw capture quota exhausted",
            ));
        }
        let n = self.sink.write(bytes)?;
        self.sha.update(&bytes[..n]);
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.sink.flush()
    }
}
