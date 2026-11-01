use super::{PublicationError, PublicationPlan};
use serde_json::Value;
use std::{collections::BTreeSet, net::SocketAddr};

pub(super) const STATUS_LIMIT: usize = 512 * 1024;
const MAX_STATUS_ENTRIES: usize = 4096;

#[derive(Clone)]
pub(crate) struct ServeStatus {
    pub(super) value: Value,
    routes: Vec<Route>,
    sessions: BTreeSet<String>,
    funnel_ports: BTreeSet<u16>,
}

#[derive(Clone)]
struct Route {
    port: u16,
    session: Option<String>,
    raw_target: Option<SocketAddr>,
}

pub(crate) struct Preflight {
    pub(super) before: ServeStatus,
    pub(super) plan: PublicationPlan,
}

#[derive(Clone)]
pub(crate) struct OwnedRoute {
    pub(super) session_id: String,
    pub(super) port: u16,
    pub(super) backend: SocketAddr,
}

impl ServeStatus {
    pub fn parse(raw: &str) -> Result<Self, PublicationError> {
        if raw.len() > STATUS_LIMIT {
            return Err(PublicationError::OutputLimitExceeded);
        }
        let value: Value =
            serde_json::from_str(raw).map_err(|_| PublicationError::StatusInvalid)?;
        if ["error", "Error"]
            .iter()
            .any(|key| value.get(*key).is_some_and(|value| !value.is_null()))
        {
            return Err(PublicationError::StatusInvalid);
        }
        let mut status = Self {
            value: value.clone(),
            routes: Vec::new(),
            sessions: BTreeSet::new(),
            funnel_ports: BTreeSet::new(),
        };
        let mut count = 0;
        read_config(&value, None, &mut status, &mut count)?;
        Ok(status)
    }
    pub fn preflight(&self, plan: &PublicationPlan) -> Result<Preflight, PublicationError> {
        if self.funnel_ports.contains(&plan.port) {
            return Err(PublicationError::FunnelConflict);
        }
        if self.routes.iter().any(|route| route.port == plan.port) {
            return Err(PublicationError::SlotOccupied);
        }
        Ok(Preflight {
            before: self.clone(),
            plan: plan.clone(),
        })
    }

    pub(super) fn port_free(&self, port: u16) -> bool {
        !self.funnel_ports.contains(&port) && !self.routes.iter().any(|route| route.port == port)
    }

    pub(super) fn equivalent_to(&self, other: &Self) -> bool {
        normalized(self.value.clone()) == normalized(other.value.clone())
    }

    pub(super) fn has_session(&self, owned: &OwnedRoute) -> bool {
        self.sessions.contains(&owned.session_id)
    }
}

impl Preflight {
    pub fn bind(
        &self,
        plan: &PublicationPlan,
        after: &ServeStatus,
    ) -> Result<OwnedRoute, PublicationError> {
        if self.plan != *plan {
            return Err(PublicationError::OwnershipUnknown);
        }
        if after.funnel_ports.contains(&plan.port) {
            return Err(PublicationError::FunnelConflict);
        }
        let matching: Vec<_> = after
            .routes
            .iter()
            .filter(|route| route.port == plan.port)
            .collect();
        let [route] = matching.as_slice() else {
            return Err(PublicationError::OwnershipUnknown);
        };
        let session = route
            .session
            .as_ref()
            .filter(|session| !self.before.sessions.contains(*session))
            .ok_or(PublicationError::OwnershipUnknown)?;
        if route.raw_target != Some(plan.backend) {
            return Err(PublicationError::OwnershipUnknown);
        }
        let owned = OwnedRoute {
            session_id: session.clone(),
            port: plan.port,
            backend: plan.backend,
        };
        if !owned.matches(after) || !after.without(&owned.session_id).equivalent_to(&self.before) {
            return Err(PublicationError::OwnershipUnknown);
        }
        Ok(owned)
    }
}

impl OwnedRoute {
    pub fn matches(&self, status: &ServeStatus) -> bool {
        if status.funnel_ports.contains(&self.port) {
            return false;
        }
        let at_port: Vec<_> = status
            .routes
            .iter()
            .filter(|route| route.port == self.port)
            .collect();
        let [route] = at_port.as_slice() else {
            return false;
        };
        route.session.as_deref() == Some(self.session_id.as_str())
            && route.raw_target == Some(self.backend)
            && status
                .routes
                .iter()
                .filter(|route| route.session.as_deref() == Some(self.session_id.as_str()))
                .count()
                == 1
    }
    /// Review-only repair plan for an exact observed owner. There is no CLI
    /// status/command compare-and-swap; NativePublication stops its exact child
    /// instead, and never executes this port-wide command automatically.
    pub fn targeted_stop_args(
        &self,
        status: &ServeStatus,
    ) -> Result<Vec<String>, PublicationError> {
        if !self.matches(status) {
            return Err(PublicationError::OwnershipLost);
        }
        Ok(vec![
            "serve".into(),
            format!("--tcp={}", self.port),
            "off".into(),
        ])
    }
    pub fn verify_removed(
        &self,
        before_stop: &ServeStatus,
        after_stop: &ServeStatus,
    ) -> Result<(), PublicationError> {
        if (!self.matches(before_stop)
            && (!before_stop.port_free(self.port)
                || before_stop.sessions.contains(&self.session_id)))
            || after_stop.sessions.contains(&self.session_id)
            || !after_stop.port_free(self.port)
        {
            return Err(PublicationError::OwnershipLost);
        }
        if !before_stop
            .without(&self.session_id)
            .equivalent_to(after_stop)
        {
            return Err(PublicationError::OwnershipUnknown);
        }
        Ok(())
    }
}

impl ServeStatus {
    fn without(&self, session: &str) -> Self {
        let mut copy = self.clone();
        if let Some(foreground) = copy
            .value
            .get_mut("Foreground")
            .and_then(Value::as_object_mut)
        {
            foreground.remove(session);
        }
        copy.sessions.remove(session);
        copy.routes
            .retain(|route| route.session.as_deref() != Some(session));
        copy
    }
}

fn normalized(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        if let Some(foreground) = object.get_mut("Foreground").and_then(Value::as_object_mut) {
            for config in foreground.values_mut() {
                *config = normalized(config.take());
            }
        }
        for key in ["TCP", "Web", "AllowFunnel", "Foreground"] {
            if object
                .get(key)
                .is_some_and(|entry| entry.as_object().is_some_and(|object| object.is_empty()))
            {
                object.remove(key);
            }
        }
    }
    value
}

fn object_field<'a>(
    value: &'a Value,
    key: &str,
) -> Result<Option<&'a serde_json::Map<String, Value>>, PublicationError> {
    value
        .get(key)
        .map(|field| field.as_object().ok_or(PublicationError::StatusInvalid))
        .transpose()
}

fn port(raw: &str) -> Result<u16, PublicationError> {
    raw.parse::<u16>()
        .ok()
        .filter(|port| *port > 0 && port.to_string() == raw)
        .ok_or(PublicationError::StatusInvalid)
}

fn host_port(raw: &str) -> Result<u16, PublicationError> {
    let (host, raw_port) = raw
        .rsplit_once(':')
        .filter(|(host, _)| !host.is_empty())
        .ok_or(PublicationError::StatusInvalid)?;
    if host.chars().any(char::is_control) {
        return Err(PublicationError::StatusInvalid);
    }
    port(raw_port)
}

fn raw_target(handler: &Value) -> Result<Option<SocketAddr>, PublicationError> {
    let object = handler.as_object().ok_or(PublicationError::StatusInvalid)?;
    for flag in ["HTTP", "HTTPS"] {
        if object.get(flag).is_some_and(|value| !value.is_boolean()) {
            return Err(PublicationError::StatusInvalid);
        }
    }
    if object
        .get("TerminateTLS")
        .is_some_and(|value| !value.is_string())
        || object
            .get("ProxyProtocol")
            .is_some_and(|value| value.as_u64().is_none())
        || object
            .get("TCPForward")
            .is_some_and(|value| !value.is_string())
    {
        return Err(PublicationError::StatusInvalid);
    }
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "TCPForward" | "HTTP" | "HTTPS" | "TerminateTLS" | "ProxyProtocol"
        )
    }) || ["HTTP", "HTTPS"]
        .iter()
        .any(|key| object.get(*key).and_then(Value::as_bool) == Some(true))
        || object
            .get("TerminateTLS")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
        || object
            .get("ProxyProtocol")
            .and_then(Value::as_u64)
            .is_some_and(|value| value != 0)
    {
        return Ok(None);
    }
    Ok(object
        .get("TCPForward")
        .and_then(Value::as_str)
        .and_then(|target| target.parse::<SocketAddr>().ok()))
}

fn read_config(
    value: &Value,
    session: Option<&str>,
    status: &mut ServeStatus,
    count: &mut usize,
) -> Result<(), PublicationError> {
    if !value.is_object() {
        return Err(PublicationError::StatusInvalid);
    }
    if let Some(tcp) = object_field(value, "TCP")? {
        for (raw_port, handler) in tcp {
            *count += 1;
            if *count > MAX_STATUS_ENTRIES {
                return Err(PublicationError::StatusInvalid);
            }
            status.routes.push(Route {
                port: port(raw_port)?,
                session: session.map(str::to_owned),
                raw_target: raw_target(handler)?,
            });
        }
    }
    if let Some(web) = object_field(value, "Web")? {
        for (address, config) in web {
            *count += 1;
            if *count > MAX_STATUS_ENTRIES || !config.is_object() {
                return Err(PublicationError::StatusInvalid);
            }
            status.routes.push(Route {
                port: host_port(address)?,
                session: session.map(str::to_owned),
                raw_target: None,
            });
        }
    }
    if let Some(funnel) = object_field(value, "AllowFunnel")? {
        for (address, enabled) in funnel {
            *count += 1;
            if *count > MAX_STATUS_ENTRIES {
                return Err(PublicationError::StatusInvalid);
            }
            let port = host_port(address)?;
            if enabled.as_bool().ok_or(PublicationError::StatusInvalid)? {
                status.funnel_ports.insert(port);
            }
        }
    }
    if let Some(foreground) = object_field(value, "Foreground")? {
        if session.is_some() {
            return Err(PublicationError::StatusInvalid);
        }
        for (id, config) in foreground {
            *count += 1;
            if *count > MAX_STATUS_ENTRIES
                || id.is_empty()
                || id.len() > 256
                || id.chars().any(char::is_control)
            {
                return Err(PublicationError::StatusInvalid);
            }
            status.sessions.insert(id.clone());
            read_config(config, Some(id), status, count)?;
        }
    }
    Ok(())
}
