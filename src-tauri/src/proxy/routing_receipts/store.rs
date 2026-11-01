use super::contract::*;
use super::{ReceiptVersion, RequestObservation};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub(crate) const RECEIPT_CAPACITY: usize = 128;
pub(crate) const RETENTION_SECONDS: u64 = 600;

pub(crate) struct ReceiptSecret([u8; 32]);
impl ReceiptSecret {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        if !lower_hex(value, 64) {
            return None;
        }
        let decoded = hex::decode(value).ok()?;
        Some(Self(decoded.try_into().ok()?))
    }
    #[cfg(test)]
    pub(crate) fn fixture(value: u8) -> Self {
        Self([value; 32])
    }
}
impl std::fmt::Debug for ReceiptSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ReceiptSecret([redacted])")
    }
}

struct StoredReceipt {
    version: ReceiptVersion,
    verifier: [u8; 32],
    observation: RequestObservation,
}

pub(crate) struct ReceiptRegistry {
    instance_id: String,
    rows: Mutex<HashMap<String, StoredReceipt>>,
}

impl ReceiptRegistry {
    pub(crate) fn new() -> Result<Arc<Self>, &'static str> {
        let mut instance = [0_u8; 16];
        getrandom::getrandom(&mut instance).map_err(|_| "receipt_entropy_unavailable")?;
        Ok(Arc::new(Self {
            instance_id: hex::encode(instance),
            rows: Mutex::new(HashMap::new()),
        }))
    }
    pub(crate) fn instance_id(&self) -> &str {
        &self.instance_id
    }

    fn verifier(
        &self,
        nonce: &str,
        secret: &ReceiptSecret,
        version: ReceiptVersion,
    ) -> Hmac<Sha256> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&secret.0).expect("fixed-length HMAC key");
        mac.update(match version {
            ReceiptVersion::V1 => b"aag-routing-receipt-v1\0",
            ReceiptVersion::V2 => b"aag-routing-receipt-v2\0",
        });
        mac.update(self.instance_id.as_bytes());
        mac.update(b"\0");
        mac.update(nonce.as_bytes());
        mac
    }

    pub(crate) fn reserve(
        &self,
        nonce: &str,
        secret: &ReceiptSecret,
        request_id: &str,
        surface: &str,
    ) -> Result<RequestObservation, &'static str> {
        self.reserve_version(nonce, secret, request_id, surface, ReceiptVersion::V1, None)
    }

    pub(crate) fn reserve_v2(
        &self,
        nonce: &str,
        secret: &ReceiptSecret,
        request_id: &str,
        surface: &str,
        expected_admission: RouteAdmission,
    ) -> Result<RequestObservation, &'static str> {
        self.reserve_version(
            nonce,
            secret,
            request_id,
            surface,
            ReceiptVersion::V2,
            Some(expected_admission),
        )
    }

    fn reserve_version(
        &self,
        nonce: &str,
        secret: &ReceiptSecret,
        request_id: &str,
        surface: &str,
        version: ReceiptVersion,
        expected_admission: Option<RouteAdmission>,
    ) -> Result<RequestObservation, &'static str> {
        if !lower_hex(nonce, 32) {
            return Err("receipt_header_invalid");
        }
        let now = Instant::now();
        let issued_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "receipt_clock_unavailable")?
            .as_millis();
        let issued_at_ms = u64::try_from(issued_at_ms).map_err(|_| "receipt_clock_unavailable")?;
        let mut rows = self
            .rows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        rows.retain(|_, row| !row.observation.expired(now));
        if rows.contains_key(nonce) {
            return Err("receipt_nonce_reused");
        }
        if rows.len() >= RECEIPT_CAPACITY {
            return Err("receipt_capacity_exhausted");
        }
        let receipt = RoutingReceipt {
            schema_version: RECEIPT_VERSION,
            issuer: "aag".into(),
            evidence_kind: "gateway_observed".into(),
            server_instance_id: self.instance_id.clone(),
            request_nonce: nonce.into(),
            request_id: request_id.into(),
            surface: surface.into(),
            generation: None,
            requested_selector: None,
            selector_state: ValueState::Missing,
            phase: ReceiptPhase::Admitted,
            http_status: None,
            body_state: BodyState::Pending,
            selected_attempt: None,
            attempts: Vec::with_capacity(MAX_ATTEMPTS),
            capture_complete: false,
            capture_truncated: false,
            upstream_model_attestation: AttestationState::Unavailable,
            issued_at_ms,
            expires_at_ms: None,
        };
        let observation = match expected_admission {
            Some(expected) => RequestObservation::with_precondition(receipt, now, Some(expected)),
            None => RequestObservation::new(receipt, now),
        };
        let verifier = self
            .verifier(nonce, secret, version)
            .finalize()
            .into_bytes()
            .into();
        rows.insert(
            nonce.into(),
            StoredReceipt {
                version,
                verifier,
                observation: observation.clone(),
            },
        );
        Ok(observation)
    }

    pub(crate) fn lookup(&self, nonce: &str, secret: &ReceiptSecret) -> Option<RoutingReceipt> {
        self.lookup_at(nonce, secret, Instant::now())
    }
    pub(crate) fn lookup_v2(
        &self,
        nonce: &str,
        secret: &ReceiptSecret,
    ) -> Option<RoutingReceiptV2> {
        self.lookup_version_at(nonce, secret, ReceiptVersion::V2, Instant::now())
            .map(|observation| observation.snapshot_v2())
    }

    fn lookup_at(
        &self,
        nonce: &str,
        secret: &ReceiptSecret,
        now: Instant,
    ) -> Option<RoutingReceipt> {
        self.lookup_version_at(nonce, secret, ReceiptVersion::V1, now)
            .map(|observation| observation.snapshot())
    }

    fn lookup_version_at(
        &self,
        nonce: &str,
        secret: &ReceiptSecret,
        version: ReceiptVersion,
        now: Instant,
    ) -> Option<RequestObservation> {
        let mut rows = self
            .rows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        rows.retain(|_, row| !row.observation.expired(now));
        let row = rows.get(nonce)?;
        if row.version != version {
            return None;
        }
        self.verifier(nonce, secret, version)
            .verify_slice(&row.verifier)
            .ok()?;
        Some(row.observation.clone())
    }
}

pub(crate) fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn possession_is_required_and_duplicate_nonce_never_rebinds() {
        let registry = ReceiptRegistry::new().unwrap();
        let nonce = "0123456789abcdef0123456789abcdef";
        registry
            .reserve(nonce, &ReceiptSecret::fixture(1), "req_a", "chat")
            .unwrap();
        assert!(registry.lookup(nonce, &ReceiptSecret::fixture(2)).is_none());
        assert!(matches!(
            registry.reserve(nonce, &ReceiptSecret::fixture(2), "req_b", "chat"),
            Err("receipt_nonce_reused")
        ));
        assert_eq!(
            registry
                .lookup(nonce, &ReceiptSecret::fixture(1))
                .unwrap()
                .request_id,
            "req_a"
        );
    }

    #[test]
    fn capacity_preserves_active_and_unexpired_terminal_records() {
        let registry = ReceiptRegistry::new().unwrap();
        for index in 0..RECEIPT_CAPACITY {
            let row = registry
                .reserve(
                    &format!("{index:032x}"),
                    &ReceiptSecret::fixture(1),
                    "req",
                    "chat",
                )
                .unwrap();
            if index % 2 == 0 {
                row.finish(Some(200), BodyState::Exhausted);
            }
        }
        assert!(matches!(
            registry.reserve(
                &format!("{:032x}", RECEIPT_CAPACITY),
                &ReceiptSecret::fixture(1),
                "next",
                "chat"
            ),
            Err("receipt_capacity_exhausted")
        ));
    }

    #[test]
    fn terminal_expiry_and_restart_are_unavailable_not_nonexecution() {
        let registry = ReceiptRegistry::new().unwrap();
        let nonce = "0123456789abcdef0123456789abcdef";
        let row = registry
            .reserve(nonce, &ReceiptSecret::fixture(1), "req", "chat")
            .unwrap();
        assert!(registry
            .lookup_at(
                nonce,
                &ReceiptSecret::fixture(1),
                Instant::now() + Duration::from_secs(RETENTION_SECONDS + 1)
            )
            .is_some());
        row.finish(Some(200), BodyState::Exhausted);
        assert!(registry
            .lookup_at(
                nonce,
                &ReceiptSecret::fixture(1),
                Instant::now() + Duration::from_secs(RETENTION_SECONDS + 1)
            )
            .is_none());
        assert!(ReceiptRegistry::new()
            .unwrap()
            .lookup(nonce, &ReceiptSecret::fixture(1))
            .is_none());
    }

    #[test]
    fn concurrent_nonce_reservation_has_one_winner() {
        let registry = ReceiptRegistry::new().unwrap();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let registry = registry.clone();
                std::thread::spawn(move || {
                    registry
                        .reserve(
                            "0123456789abcdef0123456789abcdef",
                            &ReceiptSecret::fixture(7),
                            "req",
                            "chat",
                        )
                        .is_ok()
                })
            })
            .collect();
        assert_eq!(
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .filter(|won| *won)
                .count(),
            1
        );
    }
}
