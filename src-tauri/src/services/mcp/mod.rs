//! SQLite-backed MCP registry and explicit, review-bound client synchronization.
mod client_config;
mod types;
mod validation;
#[cfg(windows)]
mod windows_permissions;

pub use types::*;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};

use crate::{
    database::{dao::mcp::McpServer, Database},
    error::AppError,
};
use sha2::{Digest, Sha256};
use validation::{
    redact_config, validate_config, validate_existing_id, validate_id, validate_label,
};

const MAX_SERVERS: usize = 128;
const REVIEW_LIFETIME_SECONDS: u64 = 300;
const MAX_PENDING_REVIEWS: usize = 16;

pub(super) fn failure(code: &str) -> AppError {
    AppError::Message(format!("mcp:{code}"))
}

#[allow(non_camel_case_types)]
struct Pending_review {
    expires: Instant,
    registry_digest: Vec<u8>,
    original: Option<Vec<u8>>,
    replacement: Vec<u8>,
    changed_count: usize,
}

#[allow(non_camel_case_types)]
pub struct Mcp_service {
    home_dir: PathBuf,
    reviews: Mutex<HashMap<String, Pending_review>>,
}

impl Mcp_service {
    pub fn new(home_dir: PathBuf) -> Self {
        Self {
            home_dir,
            reviews: Mutex::new(HashMap::new()),
        }
    }

    pub fn list(&self, db: &Database) -> Result<Vec<Mcp_server_summary>, AppError> {
        db.get_all_mcp_servers()?.iter().map(summary).collect()
    }

    pub fn save(
        &self,
        db: &Database,
        input: Mcp_server_input,
    ) -> Result<Mcp_server_summary, AppError> {
        let mut reviews = self
            .reviews
            .lock()
            .map_err(|_| failure("state_unavailable"))?;
        validate_existing_id(&input.id)?;
        validate_label(&input.name, 128)?;
        if let Some(description) = &input.description {
            validate_label(description, 1024)?;
        }
        let existing = db.get_mcp_server(&input.id)?;
        if let Some(existing) = &existing {
            if input.create_only {
                return Err(failure("server_exists"));
            }
            if input.expected_revision.as_deref() != Some(server_revision(existing)?.as_str()) {
                return Err(failure("revision_mismatch"));
            }
        }
        if existing.is_none() {
            // A revision expresses update intent. Deletion must not turn a
            // stale complete editor payload into an unintended recreation.
            if input.expected_revision.is_some() {
                return Err(failure("revision_mismatch"));
            }
            validate_id(&input.id)?;
        }
        if existing.is_none() && db.get_all_mcp_servers()?.len() >= MAX_SERVERS {
            return Err(failure("registry_limit_reached"));
        }
        let config = match input.server_config {
            Some(config) => validate_config(&config)?,
            None => validate_config(
                &existing
                    .as_ref()
                    .ok_or_else(|| failure("definition_required"))?
                    .server_config,
            )?,
        };
        let server = McpServer {
            id: input.id,
            name: input.name,
            server_config: config,
            description: input.description,
            homepage: existing.as_ref().and_then(|server| server.homepage.clone()),
            docs: existing.as_ref().and_then(|server| server.docs.clone()),
            tags: existing.map(|server| server.tags).unwrap_or_default(),
            enabled_claude: input.enabled_claude,
            enabled_codex: input.enabled_codex,
        };
        db.save_mcp_server(&server)?;
        reviews.clear();
        summary(&server)
    }

    pub fn delete(&self, db: &Database, id: &str) -> Result<(), AppError> {
        let mut reviews = self
            .reviews
            .lock()
            .map_err(|_| failure("state_unavailable"))?;
        // Existing imported identifiers may not follow the new naming rule;
        // allow removing them without ever using them as filesystem paths.
        validate_label(id, 256)?;
        if db.get_mcp_server(id)?.is_none() {
            return Err(failure("server_not_found"));
        }
        db.delete_mcp_server(id)?;
        reviews.clear();
        Ok(())
    }

    pub fn set_enabled(
        &self,
        db: &Database,
        id: &str,
        client: Mcp_client,
        enabled: bool,
    ) -> Result<Mcp_server_summary, AppError> {
        let mut reviews = self
            .reviews
            .lock()
            .map_err(|_| failure("state_unavailable"))?;
        let server = db
            .get_mcp_server(id)?
            .ok_or_else(|| failure("server_not_found"))?;
        if enabled {
            validate_existing_id(id)?;
            validate_config(&server.server_config)?;
        }
        db.toggle_mcp_app(
            id,
            match client {
                Mcp_client::Claude => "claude",
                Mcp_client::Codex => "codex",
            },
            enabled,
        )?;
        reviews.clear();
        summary(
            &db.get_mcp_server(id)?
                .ok_or_else(|| failure("server_not_found"))?,
        )
    }

    pub fn preview(&self, db: &Database, client: Mcp_client) -> Result<Mcp_sync_preview, AppError> {
        let mut reviews = self
            .reviews
            .lock()
            .map_err(|_| failure("state_unavailable"))?;
        reviews.retain(|_, review| review.expires > Instant::now());
        let servers = db.get_all_mcp_servers()?;
        if servers.len() > MAX_SERVERS {
            return Err(failure("registry_limit_reached"));
        }
        let server_ids = servers
            .iter()
            .filter(|server| match client {
                Mcp_client::Claude => server.enabled_claude,
                Mcp_client::Codex => server.enabled_codex,
            })
            .map(|server| server.id.clone())
            .collect();
        let mut warnings = vec![
            "Arguments, environment values, headers, and URLs are masked. This review is not an executable export.".into(),
            "AAG never launches MCP servers. The client may execute commands or connect to servers after restart; review and trust each definition first.".into(),
        ];
        if client == Mcp_client::Codex {
            warnings.push("Codex is manual export review only. Existing TOML, selected models, and shared desktop configuration are never read or changed.".into());
            return Ok(Mcp_sync_preview {
                client,
                format: "toml".into(),
                redacted_config: client_config::codex_preview(&servers)?,
                server_ids,
                warnings,
                review_token: None,
                expires_at: None,
                changes: vec![],
                can_apply: false,
            });
        }
        if std::env::var_os("CLAUDE_CONFIG_DIR").is_some()
            && std::env::var_os("AAG_HOME_DIR").is_none()
        {
            return Err(failure("custom_claude_config_not_supported"));
        }
        let original = client_config::read_client(&self.home_dir.join(".claude.json"))?;
        let (replacement, redacted_config, changes) =
            client_config::claude_plan(original.as_deref(), &servers)?;
        let changed_count = changes
            .iter()
            .filter(|change| change.action != "unchanged")
            .count();
        warnings.push("Close Claude Code before applying. Only user-scope aag_<id> entries represented in this registry are changed; all other settings and MCP entries are preserved.".into());
        warnings.push("Existing aag_<id> entries are replaced or removed as listed. Removing a registry entry alone does not remove a live entry: disable, review, and apply before deletion.".into());
        warnings.push("A private backup is retained beside .claude.json before overwrite. An interrupted lock requires manual review; AAG never removes another writer's lock.".into());
        if changed_count == 0 {
            return Ok(Mcp_sync_preview {
                client,
                format: "json".into(),
                redacted_config,
                server_ids,
                warnings,
                review_token: None,
                expires_at: None,
                changes,
                can_apply: false,
            });
        }
        if reviews.len() >= MAX_PENDING_REVIEWS {
            return Err(failure("too_many_pending_reviews"));
        }
        let mut random = [0u8; 24];
        getrandom::getrandom(&mut random).map_err(|_| failure("secure_random_unavailable"))?;
        let token = hex::encode(random);
        reviews.insert(
            token.clone(),
            Pending_review {
                expires: Instant::now() + Duration::from_secs(REVIEW_LIFETIME_SECONDS),
                registry_digest: registry_digest(&servers)?,
                original,
                replacement,
                changed_count,
            },
        );
        Ok(Mcp_sync_preview {
            client,
            format: "json".into(),
            redacted_config,
            server_ids,
            warnings,
            review_token: Some(token),
            expires_at: Some(
                (chrono::Utc::now() + chrono::Duration::seconds(REVIEW_LIFETIME_SECONDS as i64))
                    .to_rfc3339(),
            ),
            changes,
            can_apply: true,
        })
    }

    pub fn apply(&self, db: &Database, review_token: &str) -> Result<Mcp_sync_receipt, AppError> {
        let mut reviews = self
            .reviews
            .lock()
            .map_err(|_| failure("state_unavailable"))?;
        // Consume before validation/write: a failed or repeated apply always needs
        // a new explicit review and can never replay an old authorization.
        let review = reviews
            .remove(review_token)
            .ok_or_else(|| failure("review_missing_or_consumed"))?;
        if review.expires <= Instant::now() {
            return Err(failure("review_expired"));
        }
        if review.registry_digest != registry_digest(&db.get_all_mcp_servers()?)? {
            return Err(failure("registry_changed_since_preview"));
        }
        let backup_created = client_config::apply_claude(
            &self.home_dir.join(".claude.json"),
            &review.original,
            &review.replacement,
            review_token,
        )?;
        reviews.clear();
        Ok(Mcp_sync_receipt {
            client: Mcp_client::Claude,
            applied_server_count: review.changed_count,
            backup_created,
            restart_required: true,
        })
    }

    /// Cancelling a review releases its bounded slot immediately. Safe to repeat
    /// after cancellation, expiry, consumption, or a lost frontend response.
    pub fn discard_review(&self, review_token: &str) -> Result<(), AppError> {
        self.reviews
            .lock()
            .map_err(|_| failure("state_unavailable"))?
            .remove(review_token);
        Ok(())
    }
}

fn registry_digest(servers: &[McpServer]) -> Result<Vec<u8>, AppError> {
    Ok(Sha256::digest(serde_json::to_vec(servers)?).to_vec())
}

fn server_revision(server: &McpServer) -> Result<String, AppError> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(server)?)))
}

fn summary(server: &McpServer) -> Result<Mcp_server_summary, AppError> {
    let validation =
        validate_existing_id(&server.id).and_then(|_| validate_config(&server.server_config));
    let transport = validation
        .as_ref()
        .ok()
        .and_then(|config| config.get("type"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("invalid")
        .to_string();
    Ok(Mcp_server_summary {
        id: server.id.clone(),
        revision: server_revision(server)?,
        name: server.name.clone(),
        description: server.description.clone(),
        transport,
        enabled_claude: server.enabled_claude,
        enabled_codex: server.enabled_codex,
        redacted_config: redact_config(&server.server_config),
        validation_error: validation.err().map(|error| error.to_string()),
    })
}

#[cfg(test)]
mod tests;
