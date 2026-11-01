//! Transactional policy/index persistence; native payloads stay with their owner.

use super::policy::{classify_request, validate_inflight_request, Operation};
use super::{
    BrokerDecision, BrokerError, CanonicalSessionId, ClientRequest, IdleAttestation,
    NativeSessionId, NativeThreadId, OperatorAuthorization, PayloadObjectId, SessionSnapshot,
    ThreadReference, VerifiedOwner, WritePermit,
};
use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SCHEMA_VERSION: i64 = 1;
const SQLITE_BUSY_TIMEOUT_MS: u64 = 1000;
const IDENTITY_BYTES: usize = 16;
const REFERENCE_SELECT: &str = "SELECT s.canonical_session_id, s.owner_instance_id,
    s.native_session_id, t.native_thread_id, t.payload_object_id
    FROM broker_native_threads t JOIN broker_sessions s
      ON s.canonical_session_id = t.canonical_session_id";
const SCHEMA_SQL: &str = "
    CREATE TABLE IF NOT EXISTS broker_sessions (
        canonical_session_id TEXT PRIMARY KEY NOT NULL,
        owner_instance_id TEXT NOT NULL COLLATE BINARY,
        native_session_id TEXT NOT NULL COLLATE BINARY,
        read_only INTEGER NOT NULL DEFAULT 0 CHECK (read_only IN (0, 1)),
        epoch INTEGER NOT NULL DEFAULT 0 CHECK (epoch >= 0),
        UNIQUE (owner_instance_id, native_session_id),
        UNIQUE (canonical_session_id, owner_instance_id)
    );
    CREATE TABLE IF NOT EXISTS broker_native_threads (
        owner_instance_id TEXT NOT NULL COLLATE BINARY,
        native_thread_id TEXT NOT NULL COLLATE BINARY,
        canonical_session_id TEXT NOT NULL,
        payload_object_id TEXT NOT NULL COLLATE BINARY,
        PRIMARY KEY (owner_instance_id, native_thread_id),
        FOREIGN KEY (canonical_session_id, owner_instance_id)
          REFERENCES broker_sessions(canonical_session_id, owner_instance_id)
    );
    CREATE TABLE IF NOT EXISTS broker_write_permits (
        canonical_session_id TEXT PRIMARY KEY NOT NULL,
        permit_id TEXT NOT NULL UNIQUE,
        epoch INTEGER NOT NULL CHECK (epoch > 0),
        owner_instance_id TEXT NOT NULL COLLATE BINARY,
        native_thread_id TEXT NOT NULL COLLATE BINARY,
        method TEXT NOT NULL,
        FOREIGN KEY (canonical_session_id, owner_instance_id)
          REFERENCES broker_sessions(canonical_session_id, owner_instance_id),
        FOREIGN KEY (owner_instance_id, native_thread_id)
          REFERENCES broker_native_threads(owner_instance_id, native_thread_id)
    );";

#[derive(Clone)]
pub struct SessionRegistry {
    connection: Arc<Mutex<Connection>>,
}

impl SessionRegistry {
    /// The service owner establishes database path, ACLs, and journal/WAL/SHM
    /// safety. This constructor does not certify those external properties.
    /// The connection must be dedicated to the broker's policy/index database.
    pub fn from_connection(mut connection: Connection) -> Result<Self, BrokerError> {
        connection.busy_timeout(Duration::from_millis(SQLITE_BUSY_TIMEOUT_MS))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS broker_schema (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                version INTEGER NOT NULL
             );",
        )?;
        transaction.execute(
            "INSERT INTO broker_schema (singleton, version) VALUES (1, ?1)
             ON CONFLICT(singleton) DO NOTHING",
            [SCHEMA_VERSION],
        )?;
        let version: i64 = transaction.query_row(
            "SELECT version FROM broker_schema WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        if version != SCHEMA_VERSION {
            return Err(BrokerError::UnsupportedSchema);
        }
        transaction.execute_batch(SCHEMA_SQL)?;
        transaction.commit()?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Register an owner-observed native object. Repeated exact observations are
    /// idempotent; an alias or payload conflict never replaces an existing row.
    /// No native path, database, message, or attachment is opened here.
    pub fn register_thread(
        &self,
        owner: &VerifiedOwner,
        native_session_id: &NativeSessionId,
        native_thread_id: &NativeThreadId,
        payload_object_id: &PayloadObjectId,
    ) -> Result<ThreadReference, BrokerError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| BrokerError::LockFailed)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = find_reference(&transaction, owner, native_thread_id)? {
            if existing.native_session_id != *native_session_id {
                return Err(BrokerError::AliasConflict);
            }
            if existing.payload_object_id != *payload_object_id {
                return Err(BrokerError::PayloadConflict);
            }
            transaction.commit()?;
            return Ok(existing);
        }

        let existing_session: Option<String> = transaction
            .query_row(
                "SELECT canonical_session_id FROM broker_sessions
                 WHERE owner_instance_id = ?1 AND native_session_id = ?2",
                params![owner.owner_instance_id, native_session_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let canonical_session_id = match existing_session {
            Some(value) => CanonicalSessionId(value),
            None => {
                let value = CanonicalSessionId(new_identity("session")?);
                transaction.execute(
                    "INSERT INTO broker_sessions
                        (canonical_session_id, owner_instance_id, native_session_id)
                     VALUES (?1, ?2, ?3)",
                    params![
                        value.as_str(),
                        owner.owner_instance_id,
                        native_session_id.as_str()
                    ],
                )?;
                value
            }
        };
        let snapshot = load_snapshot(&transaction, owner, &canonical_session_id)?;
        if snapshot.admitted_work {
            return Err(BrokerError::Busy);
        }
        transaction.execute(
            "INSERT INTO broker_native_threads
                (owner_instance_id, native_thread_id, canonical_session_id, payload_object_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                owner.owner_instance_id,
                native_thread_id.as_str(),
                canonical_session_id.as_str(),
                payload_object_id.as_str(),
            ],
        )?;
        advance_epoch(&transaction, &snapshot)?;
        transaction.commit()?;
        Ok(ThreadReference {
            canonical_session_id,
            owner_instance_id: owner.owner_instance_id.clone(),
            native_session_id: native_session_id.clone(),
            native_thread_id: native_thread_id.clone(),
            payload_object_id: payload_object_id.clone(),
        })
    }

    pub fn resolve_thread(
        &self,
        owner: &VerifiedOwner,
        native_thread_id: &NativeThreadId,
    ) -> Result<ThreadReference, BrokerError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| BrokerError::LockFailed)?;
        find_reference(&connection, owner, native_thread_id)?.ok_or(BrokerError::UnknownThread)
    }

    pub fn list_threads(
        &self,
        owner: &VerifiedOwner,
        canonical_session_id: &CanonicalSessionId,
    ) -> Result<Vec<ThreadReference>, BrokerError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| BrokerError::LockFailed)?;
        load_snapshot(&connection, owner, canonical_session_id)?;
        let mut statement = connection.prepare(&format!(
            "{REFERENCE_SELECT} WHERE s.owner_instance_id = ?1
             AND s.canonical_session_id = ?2 ORDER BY t.native_thread_id"
        ))?;
        let rows = statement.query_map(
            params![owner.owner_instance_id, canonical_session_id.as_str()],
            map_reference,
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn session_snapshot(
        &self,
        owner: &VerifiedOwner,
        canonical_session_id: &CanonicalSessionId,
    ) -> Result<SessionSnapshot, BrokerError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| BrokerError::LockFailed)?;
        load_snapshot(&connection, owner, canonical_session_id)
    }

    /// The adapter must forward the approved request without substituting caller
    /// paths, another native thread, or privileged execution configuration.
    pub fn assess_client_request(
        &self,
        owner: &VerifiedOwner,
        request: ClientRequest<'_>,
    ) -> Result<BrokerDecision, BrokerError> {
        let operation = classify_request(&request)?;
        if operation == Operation::ReadGlobal {
            return Ok(BrokerDecision::Read { reference: None });
        }
        let native_thread_id = NativeThreadId::new(
            request
                .params
                .get("threadId")
                .and_then(|value| value.as_str())
                .ok_or(BrokerError::InvalidRequest)?,
        )?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| BrokerError::LockFailed)?;
        if operation == Operation::ReadThread {
            let reference = find_reference(&connection, owner, &native_thread_id)?
                .ok_or(BrokerError::UnknownThread)?;
            return Ok(BrokerDecision::Read {
                reference: Some(reference),
            });
        }
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let reference = find_reference(&transaction, owner, &native_thread_id)?
            .ok_or(BrokerError::UnknownThread)?;
        let snapshot = load_snapshot(&transaction, owner, &reference.canonical_session_id)?;
        if snapshot.read_only {
            return if operation == Operation::Resume {
                Ok(BrokerDecision::ReadProjection { reference })
            } else {
                Err(BrokerError::ReadOnly)
            };
        }
        if snapshot.admitted_work {
            return Err(BrokerError::Busy);
        }
        let epoch = advance_epoch(&transaction, &snapshot)?;
        let permit_id = new_identity("permit")?;
        transaction.execute(
            "INSERT INTO broker_write_permits
                (canonical_session_id, permit_id, epoch, owner_instance_id, native_thread_id, method)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                reference.canonical_session_id.as_str(),
                permit_id,
                epoch as i64,
                owner.owner_instance_id,
                native_thread_id.as_str(),
                request.method,
            ],
        )?;
        transaction.commit()?;
        Ok(BrokerDecision::Write(WritePermit {
            reference,
            epoch,
            permit_id,
        }))
    }

    /// Derive this thread from the trusted pending server-request association,
    /// never a client-supplied reply field. A reply retains its turn admission;
    /// it does not acquire or release a second writer permit.
    pub fn authorize_tool_reply(
        &self,
        owner: &VerifiedOwner,
        native_thread_id: &NativeThreadId,
        permit: &WritePermit,
    ) -> Result<(), BrokerError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| BrokerError::LockFailed)?;
        let reference = find_reference(&connection, owner, native_thread_id)?
            .ok_or(BrokerError::UnknownThread)?;
        let snapshot = load_snapshot(&connection, owner, &reference.canonical_session_id)?;
        if snapshot.read_only {
            return Err(BrokerError::ReadOnly);
        }
        if reference != permit.reference {
            return Err(BrokerError::StaleFence);
        }
        check_permit(&connection, permit)
    }

    /// Authorize only a steer or interrupt of the already-admitted native turn.
    /// The owner, thread binding, and permit come from the transport's active
    /// operation, not fields claiming authority in the client request. This
    /// check neither starts another admission nor releases the existing one.
    pub fn authorize_inflight_request(
        &self,
        owner: &VerifiedOwner,
        native_thread_id: &NativeThreadId,
        request: ClientRequest<'_>,
        permit: &WritePermit,
    ) -> Result<(), BrokerError> {
        validate_inflight_request(&request)?;
        let requested_thread_id = NativeThreadId::new(
            request
                .params
                .get("threadId")
                .and_then(|value| value.as_str())
                .ok_or(BrokerError::InvalidRequest)?,
        )?;
        if requested_thread_id != *native_thread_id {
            return Err(BrokerError::InvalidRequest);
        }
        self.authorize_tool_reply(owner, native_thread_id, permit)
    }

    /// The operator capability comes from the OS boundary, not an API-key role
    /// or JSON boolean. Native quiescence must hold from attestation through
    /// this call. All broker admissions serialize with this transaction.
    pub fn set_read_only(
        &self,
        authorization: &OperatorAuthorization,
        attestation: &IdleAttestation,
        read_only: bool,
    ) -> Result<SessionSnapshot, BrokerError> {
        super::validate_identifier(authorization.principal_id())?;
        let owner = VerifiedOwner {
            owner_instance_id: attestation.owner_instance_id.clone(),
        };
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| BrokerError::LockFailed)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = load_snapshot(&transaction, &owner, &attestation.canonical_session_id)?;
        if snapshot.admitted_work {
            return Err(BrokerError::Busy);
        }
        if snapshot.epoch != attestation.epoch {
            return Err(BrokerError::StaleFence);
        }
        if snapshot.read_only != read_only {
            snapshot.epoch = advance_epoch(&transaction, &snapshot)?;
            transaction.execute(
                "UPDATE broker_sessions SET read_only = ?1 WHERE canonical_session_id = ?2",
                params![read_only, snapshot.canonical_session_id.as_str()],
            )?;
            snapshot.read_only = read_only;
        }
        transaction.commit()?;
        Ok(snapshot)
    }

    pub fn validate_write_permit(&self, permit: &WritePermit) -> Result<(), BrokerError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| BrokerError::LockFailed)?;
        check_permit(&connection, permit)
    }

    /// Release only after native terminal evidence. A dropped client or cancel
    /// acknowledgement is insufficient. Unresolved permits survive reopening.
    pub fn finish_write(&self, permit: &WritePermit) -> Result<(), BrokerError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| BrokerError::LockFailed)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_permit(&transaction, permit)?;
        let owner = VerifiedOwner {
            owner_instance_id: permit.reference.owner_instance_id.clone(),
        };
        let snapshot = load_snapshot(&transaction, &owner, &permit.reference.canonical_session_id)?;
        transaction.execute(
            "DELETE FROM broker_write_permits WHERE permit_id = ?1 AND epoch = ?2",
            params![permit.permit_id, permit.epoch as i64],
        )?;
        advance_epoch(&transaction, &snapshot)?;
        transaction.commit()?;
        Ok(())
    }
}

fn map_reference(row: &Row<'_>) -> rusqlite::Result<ThreadReference> {
    Ok(ThreadReference {
        canonical_session_id: CanonicalSessionId(row.get(0)?),
        owner_instance_id: row.get(1)?,
        native_session_id: NativeSessionId(row.get(2)?),
        native_thread_id: NativeThreadId(row.get(3)?),
        payload_object_id: PayloadObjectId(row.get(4)?),
    })
}

fn find_reference(
    connection: &Connection,
    owner: &VerifiedOwner,
    native_thread_id: &NativeThreadId,
) -> Result<Option<ThreadReference>, BrokerError> {
    Ok(connection
        .query_row(
            &format!(
                "{REFERENCE_SELECT} WHERE t.owner_instance_id = ?1 AND t.native_thread_id = ?2"
            ),
            params![owner.owner_instance_id, native_thread_id.as_str()],
            map_reference,
        )
        .optional()?)
}

fn load_snapshot(
    connection: &Connection,
    owner: &VerifiedOwner,
    canonical_session_id: &CanonicalSessionId,
) -> Result<SessionSnapshot, BrokerError> {
    connection
        .query_row(
            "SELECT canonical_session_id, owner_instance_id, native_session_id, read_only, epoch,
            EXISTS(SELECT 1 FROM broker_write_permits p
                   WHERE p.canonical_session_id = s.canonical_session_id)
         FROM broker_sessions s WHERE canonical_session_id = ?1 AND owner_instance_id = ?2",
            params![canonical_session_id.as_str(), owner.owner_instance_id],
            |row| {
                Ok(SessionSnapshot {
                    canonical_session_id: CanonicalSessionId(row.get(0)?),
                    owner_instance_id: row.get(1)?,
                    native_session_id: NativeSessionId(row.get(2)?),
                    read_only: row.get(3)?,
                    epoch: row.get::<_, i64>(4)? as u64,
                    admitted_work: row.get(5)?,
                })
            },
        )
        .optional()?
        .ok_or(BrokerError::UnknownThread)
}

fn advance_epoch(connection: &Connection, snapshot: &SessionSnapshot) -> Result<u64, BrokerError> {
    let epoch = snapshot
        .epoch
        .checked_add(1)
        .filter(|value| *value <= i64::MAX as u64)
        .ok_or(BrokerError::EpochExhausted)?;
    let changed = connection.execute(
        "UPDATE broker_sessions SET epoch = ?1 WHERE canonical_session_id = ?2 AND epoch = ?3",
        params![
            epoch as i64,
            snapshot.canonical_session_id.as_str(),
            snapshot.epoch as i64
        ],
    )?;
    if changed != 1 {
        return Err(BrokerError::StaleFence);
    }
    Ok(epoch)
}

fn check_permit(connection: &Connection, permit: &WritePermit) -> Result<(), BrokerError> {
    if permit.epoch > i64::MAX as u64 {
        return Err(BrokerError::StaleFence);
    }
    let valid: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM broker_write_permits p
            JOIN broker_sessions s ON s.canonical_session_id = p.canonical_session_id
            JOIN broker_native_threads t ON t.owner_instance_id = p.owner_instance_id
                AND t.native_thread_id = p.native_thread_id
            WHERE p.permit_id = ?1 AND p.epoch = ?2 AND s.epoch = p.epoch
              AND s.read_only = 0 AND s.canonical_session_id = ?3
              AND s.owner_instance_id = ?4 AND s.native_session_id = ?5
              AND t.native_thread_id = ?6 AND t.payload_object_id = ?7
              AND t.canonical_session_id = s.canonical_session_id
        )",
        params![
            permit.permit_id,
            permit.epoch as i64,
            permit.reference.canonical_session_id.as_str(),
            permit.reference.owner_instance_id,
            permit.reference.native_session_id.as_str(),
            permit.reference.native_thread_id.as_str(),
            permit.reference.payload_object_id.as_str(),
        ],
        |row| row.get(0),
    )?;
    if valid {
        Ok(())
    } else {
        Err(BrokerError::StaleFence)
    }
}

fn new_identity(kind: &str) -> Result<String, BrokerError> {
    let mut bytes = [0u8; IDENTITY_BYTES];
    getrandom::getrandom(&mut bytes).map_err(|_| BrokerError::RandomIdentity)?;
    Ok(format!("{kind}_{}", hex::encode(bytes)))
}
