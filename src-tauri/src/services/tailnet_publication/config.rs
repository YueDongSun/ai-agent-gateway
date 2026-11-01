use super::PublicationError;
use serde::{Deserialize, Serialize};
use std::{fmt, net::SocketAddr};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawConfig")]
pub(crate) struct PublicationConfig {
    pub enabled: bool,
    pub port: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    enabled: bool,
    port: u32,
}

impl TryFrom<RawConfig> for PublicationConfig {
    type Error = PublicationError;
    fn try_from(raw: RawConfig) -> Result<Self, Self::Error> {
        let port = u16::try_from(raw.port)
            .ok()
            .filter(|port| *port > 0)
            .ok_or(PublicationError::PortInvalid)?;
        Ok(Self {
            enabled: raw.enabled,
            port,
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PublicationPlan {
    pub(super) port: u16,
    pub(super) backend: SocketAddr,
}

impl PublicationConfig {
    pub fn plan(
        &self,
        existing_key: &str,
        backend: &str,
    ) -> Result<PublicationPlan, PublicationError> {
        if !self.enabled {
            return Err(PublicationError::Disabled);
        }
        if self.port == 0 {
            return Err(PublicationError::PortInvalid);
        }
        if existing_key.trim().is_empty() {
            return Err(PublicationError::ExistingKeyRequired);
        }
        let backend: SocketAddr = backend
            .parse()
            .map_err(|_| PublicationError::BackendInvalid)?;
        if !backend.ip().is_loopback() || backend.port() == 0 {
            return Err(PublicationError::BackendInvalid);
        }
        Ok(PublicationPlan {
            port: self.port,
            backend,
        })
    }
}

impl PublicationPlan {
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn backend(&self) -> SocketAddr {
        self.backend
    }
    pub fn foreground_args(&self) -> Vec<String> {
        vec![
            "serve".into(),
            format!("--tcp={}", self.port),
            format!("tcp://{}", self.backend),
        ]
    }
    pub fn status_args() -> Vec<String> {
        vec!["serve".into(), "status".into(), "--json".into()]
    }
}

impl fmt::Debug for PublicationPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublicationPlan")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}
