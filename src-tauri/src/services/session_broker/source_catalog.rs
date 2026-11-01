//! Persistent physical-source generations. This index contains references only.
//!
//! The authority serializes native admission and obtains independent filesystem,
//! account and quiescence evidence. A pending cutover is never an active source.

use super::{CanonicalSessionId, ThreadReference};
use crate::config::private_fs::Managed_directory_identity;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

const SCHEMA_VERSION: i64 = 1;
const SQLITE_BUSY_MS: u64 = 1000;

#[derive(Debug, thiserror::Error)]
pub(crate) enum CatalogError {
    #[error("source_catalog_invalid_input")]
    InvalidInput,
    #[error("source_catalog_conflict")]
    Conflict,
    #[error("source_catalog_generation_exhausted")]
    GenerationExhausted,
    #[error("source_catalog_recovery_required")]
    RecoveryRequired,
    #[error("source_catalog_stale_generation")]
    StaleGeneration,
    #[error("source_catalog_lock_failed")]
    LockFailed,
    #[error("source_catalog_storage_failed")]
    Storage(#[from] rusqlite::Error),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceKey(String);

impl SourceKey {
    pub fn generate() -> Result<Self, CatalogError> {
        let mut bytes = [0u8; 16];
        getrandom::getrandom(&mut bytes).map_err(|_| CatalogError::InvalidInput)?;
        Ok(Self(hex::encode(bytes)))
    }
    pub fn parse(value: &str) -> Result<Self, CatalogError> {
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(CatalogError::InvalidInput);
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceMode {
    Writable,
    ReadOnly,
}

impl SourceMode {
    fn value(self) -> i64 {
        if self == Self::ReadOnly {
            1
        } else {
            0
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceBinding {
    pub source_key: SourceKey,
    pub canonical_session_id: CanonicalSessionId,
    pub native_root: PathBuf,
    pub root_identity: Managed_directory_identity,
    pub physical_generation: u64,
    pub mode: SourceMode,
    pub runtime_sid: Option<String>,
}

/// Constructed only after the owner checks the actual root object, A ownership,
/// C read access and the exact B-generation ACL. It is deliberately not Serde.
pub(crate) struct VerifiedSourceRoot {
    pub(super) path: PathBuf,
    pub(super) identity: Managed_directory_identity,
    pub(super) mode: SourceMode,
    pub(super) runtime_sid: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceTransition {
    pub source_key: SourceKey,
    pub transition_id: String,
    pub original_generation: u64,
    pub target_generation: u64,
    pub target_mode: SourceMode,
}

#[derive(Clone)]
pub(crate) struct SourceCatalog {
    connection: Arc<Mutex<Connection>>,
}

impl SourceCatalog {
    /// The service establishes file/WAL/journal ownership before opening this
    /// dedicated metadata database. No native message or attachment is stored.
    pub fn from_connection(mut connection: Connection) -> Result<Self, CatalogError> {
        connection.busy_timeout(Duration::from_millis(SQLITE_BUSY_MS))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch("CREATE TABLE IF NOT EXISTS source_schema (singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS physical_sources (
                source_key TEXT PRIMARY KEY NOT NULL, canonical_id TEXT UNIQUE NOT NULL,
                owner_instance_id TEXT NOT NULL, native_session_id TEXT NOT NULL,
                native_root TEXT NOT NULL UNIQUE, volume_id TEXT NOT NULL, object_id TEXT NOT NULL,
                generation INTEGER NOT NULL CHECK(generation>0), read_only INTEGER NOT NULL CHECK(read_only IN(0,1)),
                runtime_sid TEXT, transition_id TEXT, target_generation INTEGER, target_read_only INTEGER, last_transition_id TEXT,
                UNIQUE(owner_instance_id,native_session_id), UNIQUE(volume_id,object_id));
            CREATE TABLE IF NOT EXISTS source_runtime_principals (
                runtime_sid TEXT PRIMARY KEY NOT NULL, source_key TEXT NOT NULL, generation INTEGER NOT NULL,
                UNIQUE(source_key,generation), FOREIGN KEY(source_key) REFERENCES physical_sources(source_key));")?;
        transaction.execute("INSERT INTO source_schema(singleton,version) VALUES(1,?1) ON CONFLICT(singleton) DO NOTHING", [SCHEMA_VERSION])?;
        let version: i64 = transaction.query_row(
            "SELECT version FROM source_schema WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        if version != SCHEMA_VERSION {
            return Err(CatalogError::Conflict);
        }
        transaction.commit()?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn register(
        &self,
        source_key: &SourceKey,
        reference: &ThreadReference,
        root: VerifiedSourceRoot,
    ) -> Result<SourceBinding, CatalogError> {
        let path = validate_root(&root)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| CatalogError::LockFailed)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(current) = load(&transaction, source_key)? {
            let native_binding: (String,String) = transaction.query_row("SELECT owner_instance_id,native_session_id FROM physical_sources WHERE source_key=?1",[source_key.as_str()],|row|Ok((row.get(0)?,row.get(1)?)))?;
            if current.canonical_session_id != reference.canonical_session_id
                || current.native_root != root.path
                || current.root_identity != root.identity
                || current.mode != root.mode
                || current.runtime_sid != root.runtime_sid
                || native_binding
                    != (
                        reference.owner_instance_id.clone(),
                        reference.native_session_id.as_str().to_owned(),
                    )
            {
                return Err(CatalogError::Conflict);
            }
            ensure_active(&transaction, source_key)?;
            return Ok(current);
        }
        transaction.execute("INSERT INTO physical_sources(source_key,canonical_id,owner_instance_id,native_session_id,native_root,volume_id,object_id,generation,read_only,runtime_sid)
            VALUES(?1,?2,?3,?4,?5,?6,?7,1,?8,?9)", params![source_key.as_str(), reference.canonical_session_id.as_str(),reference.owner_instance_id,reference.native_session_id.as_str(),path,
                root.identity.volume.to_string(),root.identity.object.to_string(),root.mode.value(),root.runtime_sid])?;
        if let Some(sid) = &root.runtime_sid {
            claim_principal(&transaction, source_key, 1, sid)?;
        }
        transaction.commit()?;
        drop(connection);
        self.active(source_key)
    }

    pub fn active(&self, source_key: &SourceKey) -> Result<SourceBinding, CatalogError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| CatalogError::LockFailed)?;
        ensure_active(&connection, source_key)?;
        load(&connection, source_key)?.ok_or(CatalogError::Conflict)
    }

    /// Call under the source's outer native-admission gate. The durable pending
    /// record must precede stopping B, copying/sealing, ACL changes or renames.
    pub fn begin(
        &self,
        source_key: &SourceKey,
        expected_generation: u64,
        target_mode: SourceMode,
    ) -> Result<SourceTransition, CatalogError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| CatalogError::LockFailed)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_active(&transaction, source_key)?;
        let current = load(&transaction, source_key)?.ok_or(CatalogError::Conflict)?;
        if current.physical_generation != expected_generation {
            return Err(CatalogError::StaleGeneration);
        }
        if current.mode == target_mode {
            return Err(CatalogError::Conflict);
        }
        let target_generation = expected_generation
            .checked_add(1)
            .filter(|value| *value <= i64::MAX as u64)
            .ok_or(CatalogError::GenerationExhausted)?;
        let transition_id = SourceKey::generate()?.0;
        transaction.execute("UPDATE physical_sources SET transition_id=?2,target_generation=?3,target_read_only=?4 WHERE source_key=?1",
            params![source_key.as_str(),transition_id,target_generation as i64,target_mode.value()])?;
        transaction.commit()?;
        Ok(SourceTransition {
            source_key: source_key.clone(),
            transition_id,
            original_generation: expected_generation,
            target_generation,
            target_mode,
        })
    }

    /// Recovery returns the exact persisted intent; it never clears one because
    /// the caller restarted or a native job is no longer visible.
    pub fn pending(
        &self,
        source_key: &SourceKey,
    ) -> Result<Option<SourceTransition>, CatalogError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| CatalogError::LockFailed)?;
        connection.query_row("SELECT transition_id,generation,target_generation,target_read_only FROM physical_sources WHERE source_key=?1 AND transition_id IS NOT NULL",[source_key.as_str()],|row| {
            let mode: i64 = row.get(3)?;
            Ok(SourceTransition { source_key:source_key.clone(),transition_id:row.get(0)?,original_generation:row.get::<_,i64>(1)? as u64,target_generation:row.get::<_,i64>(2)? as u64,target_mode:if mode==1 {SourceMode::ReadOnly} else {SourceMode::Writable} })
        }).optional().map_err(Into::into)
    }

    /// Commit only a new A-owned physical object for sealing. Reopening can
    /// retain that sealed object, but must admit a never-used normal B SID.
    pub fn commit(
        &self,
        intent: &SourceTransition,
        root: VerifiedSourceRoot,
    ) -> Result<SourceBinding, CatalogError> {
        let path = validate_root(&root)?;
        if intent.target_generation > i64::MAX as u64
            || intent.target_generation
                != intent
                    .original_generation
                    .checked_add(1)
                    .ok_or(CatalogError::GenerationExhausted)?
        {
            return Err(CatalogError::StaleGeneration);
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| CatalogError::LockFailed)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = load(&transaction, &intent.source_key)?.ok_or(CatalogError::Conflict)?;
        let pending: (Option<String>,Option<i64>,Option<i64>) = transaction.query_row("SELECT transition_id,target_generation,target_read_only FROM physical_sources WHERE source_key=?1",[intent.source_key.as_str()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
        if pending == (None, None, None)
            && current.physical_generation == intent.target_generation
            && current.native_root == root.path
            && current.root_identity == root.identity
            && current.mode == root.mode
            && current.runtime_sid == root.runtime_sid
        {
            let last: Option<String> = transaction.query_row(
                "SELECT last_transition_id FROM physical_sources WHERE source_key=?1",
                [intent.source_key.as_str()],
                |row| row.get(0),
            )?;
            if last.as_deref() == Some(intent.transition_id.as_str()) {
                return Ok(current);
            }
        }
        if pending
            != (
                Some(intent.transition_id.clone()),
                Some(intent.target_generation as i64),
                Some(intent.target_mode.value()),
            )
            || current.physical_generation != intent.original_generation
            || root.path != current.native_root
            || root.mode != intent.target_mode
            || intent.target_generation
                != intent
                    .original_generation
                    .checked_add(1)
                    .ok_or(CatalogError::GenerationExhausted)?
        {
            return Err(CatalogError::StaleGeneration);
        }
        if root.identity.volume != current.root_identity.volume
            || (root.mode == SourceMode::ReadOnly && root.identity == current.root_identity)
        {
            return Err(CatalogError::Conflict);
        }
        if let Some(sid) = &root.runtime_sid {
            claim_principal(
                &transaction,
                &intent.source_key,
                intent.target_generation,
                sid,
            )?;
        }
        transaction.execute("UPDATE physical_sources SET native_root=?2,volume_id=?3,object_id=?4,generation=?5,read_only=?6,runtime_sid=?7,transition_id=NULL,target_generation=NULL,target_read_only=NULL,last_transition_id=?8 WHERE source_key=?1",
            params![intent.source_key.as_str(),path,root.identity.volume.to_string(),root.identity.object.to_string(),intent.target_generation as i64,root.mode.value(),root.runtime_sid,intent.transition_id])?;
        transaction.commit()?;
        drop(connection);
        self.active(&intent.source_key)
    }
}

fn validate_root(root: &VerifiedSourceRoot) -> Result<String, CatalogError> {
    let path = root.path.to_str().ok_or(CatalogError::InvalidInput)?;
    if !root.path.is_absolute()
        || path.chars().any(char::is_control)
        || root.path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
        || (root.mode == SourceMode::ReadOnly) != root.runtime_sid.is_none()
    {
        return Err(CatalogError::InvalidInput);
    }
    if let Some(sid) = &root.runtime_sid {
        let parts: Vec<_> = sid.split('-').collect();
        if parts.len() != 9
            || parts[..4] != ["S", "1", "5", "80"]
            || parts[4..].iter().any(|part| {
                part.is_empty()
                    || part
                        .parse::<u32>()
                        .map(|value| value.to_string() != *part)
                        .unwrap_or(true)
            })
        {
            return Err(CatalogError::InvalidInput);
        }
    }
    Ok(path.to_owned())
}

fn ensure_active(connection: &Connection, source_key: &SourceKey) -> Result<(), CatalogError> {
    let pending: Option<Option<String>> = connection
        .query_row(
            "SELECT transition_id FROM physical_sources WHERE source_key=?1",
            [source_key.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    match pending {
        Some(None) => Ok(()),
        Some(Some(_)) => Err(CatalogError::RecoveryRequired),
        None => Err(CatalogError::Conflict),
    }
}

fn load(connection: &Connection, key: &SourceKey) -> Result<Option<SourceBinding>, CatalogError> {
    connection.query_row("SELECT canonical_id,native_root,volume_id,object_id,generation,read_only,runtime_sid FROM physical_sources WHERE source_key=?1",[key.as_str()],|row| {
        let volume:String=row.get(2)?; let object:String=row.get(3)?;
        let parse_id=|value:String|value.parse::<u64>().map_err(|error|rusqlite::Error::FromSqlConversionFailure(0,rusqlite::types::Type::Text,Box::new(error)));
        Ok(SourceBinding {source_key:key.clone(),canonical_session_id:CanonicalSessionId(row.get(0)?),native_root:PathBuf::from(row.get::<_,String>(1)?),root_identity:Managed_directory_identity {volume:parse_id(volume)?,object:parse_id(object)?},physical_generation:row.get::<_,i64>(4)? as u64,mode:if row.get::<_,i64>(5)?==1{SourceMode::ReadOnly}else{SourceMode::Writable},runtime_sid:row.get(6)?})
    }).optional().map_err(Into::into)
}

fn claim_principal(
    connection: &Connection,
    key: &SourceKey,
    generation: u64,
    sid: &str,
) -> Result<(), CatalogError> {
    let used:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM source_runtime_principals WHERE runtime_sid=?1 OR (source_key=?2 AND generation=?3))",params![sid,key.as_str(),generation as i64],|row|row.get(0))?;
    if used {
        return Err(CatalogError::Conflict);
    }
    connection.execute(
        "INSERT INTO source_runtime_principals(runtime_sid,source_key,generation) VALUES(?1,?2,?3)",
        params![sid, key.as_str(), generation as i64],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
