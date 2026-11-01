use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashSet},
    net::IpAddr,
};

pub(super) const STATUS_LIMIT: usize = 1024 * 1024;
const MAX_PEER_DOCUMENTS: usize = 512;
const MAX_CHECKED_PEERS: usize = 64;
const MAX_ADDRESSES: usize = 16;

#[derive(Clone, Debug)]
pub(super) struct Candidate {
    pub device_name: String,
    pub address: IpAddr,
    pub is_self: bool,
}

pub(super) struct Inventory {
    pub candidates: Vec<Candidate>,
    pub peer_count: usize,
    pub skipped: usize,
    pub limited: bool,
}

#[derive(Deserialize)]
struct Status_document {
    #[serde(rename = "BackendState")]
    backend_state: String,
    #[serde(rename = "Self")]
    own: Option<Node_document>,
    #[serde(rename = "Peer", default)]
    peers: BTreeMap<String, Node_document>,
}

#[derive(Deserialize)]
struct Node_document {
    #[serde(rename = "HostName", default)]
    host_name: String,
    #[serde(rename = "Online", default)]
    online: bool,
    #[serde(rename = "TailscaleIPs", default)]
    addresses: Vec<String>,
}

pub(super) fn is_tailnet_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => u32::from(address) & 0xffc0_0000 == 0x6440_0000,
        IpAddr::V6(address) => address.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

fn candidate(node: Node_document, is_self: bool) -> Result<Option<Candidate>, String> {
    if node.addresses.len() > MAX_ADDRESSES {
        return Err("tailnet_discovery_status_invalid".into());
    }
    // Reject the whole inventory on an injected non-overlay address. Physical
    // endpoints, relay addresses and DNS names are never probe destinations.
    let mut addresses = node
        .addresses
        .into_iter()
        .map(|value| {
            value
                .parse::<IpAddr>()
                .ok()
                .filter(|address| is_tailnet_address(*address))
                .ok_or_else(|| "tailnet_discovery_status_invalid".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    addresses.sort_by_key(|address| (address.is_ipv6(), *address));
    if !is_self && !node.online {
        return Ok(None);
    }
    let Some(address) = addresses.into_iter().next() else {
        return Ok(None);
    };
    let device_name = if node.host_name.trim().is_empty()
        || node.host_name.len() > 128
        || node.host_name.chars().any(char::is_control)
    {
        "Unnamed device".into()
    } else {
        node.host_name
    };
    Ok(Some(Candidate {
        device_name,
        address,
        is_self,
    }))
}

pub(super) fn parse_inventory(raw: &[u8]) -> Result<Inventory, String> {
    if raw.len() > STATUS_LIMIT {
        return Err("tailnet_discovery_status_too_large".into());
    }
    let document: Status_document =
        serde_json::from_slice(raw).map_err(|_| "tailnet_discovery_status_invalid".to_string())?;
    if document.backend_state != "Running" {
        return Err("tailnet_discovery_stopped".into());
    }
    if document.peers.len() > MAX_PEER_DOCUMENTS {
        return Err("tailnet_discovery_peer_limit".into());
    }
    let own = document
        .own
        .ok_or_else(|| "tailnet_discovery_status_invalid".to_string())?;
    let peer_count = document.peers.len() + 1;
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    if let Some(own) = candidate(own, true)? {
        seen.insert(own.address);
        candidates.push(own);
    }
    for node in document.peers.into_values() {
        if let Some(node) = candidate(node, false)? {
            if seen.insert(node.address) {
                candidates.push(node);
            }
        }
    }
    let admitted = candidates.len();
    let limited = admitted > MAX_CHECKED_PEERS;
    candidates.truncate(MAX_CHECKED_PEERS);
    let skipped = peer_count.saturating_sub(candidates.len());
    Ok(Inventory {
        candidates,
        peer_count,
        skipped,
        limited,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn admits_only_running_inventory_online_peers_and_unique_overlay_addresses() {
        let inventory = parse_inventory(&serde_json::to_vec(&json!({
            "BackendState":"Running", "Self":{"HostName":"fixture-this-device","TailscaleIPs":["fd7a:115c:a1e0::1","100.64.0.1"]},
            "Peer":{
                "a":{"HostName":"fixture-online-device","Online":true,"TailscaleIPs":["100.64.0.2"]},
                "b":{"HostName":"fixture-duplicate","Online":true,"TailscaleIPs":["100.64.0.2"]},
                "c":{"HostName":"fixture-offline","Online":false,"TailscaleIPs":["100.64.0.3"]},
                "d":{"HostName":"fixture-unaddressed","Online":true,"TailscaleIPs":[]}
            }, "Endpoints":["198.51.100.1"]
        })).unwrap()).unwrap();
        assert_eq!(inventory.peer_count, 5);
        assert_eq!(inventory.candidates.len(), 2);
        // Fixture self-address preference is IPv4 before IPv6.
        assert_eq!(
            inventory.candidates[0].address,
            "100.64.0.1".parse::<IpAddr>().unwrap()
        );
        assert!(inventory.candidates[0].is_self);
        assert_eq!(inventory.candidates[1].device_name, "fixture-online-device");
        // The duplicate, offline and unaddressed records are counted but are
        // never admitted as probe destinations. Endpoints is not an inventory.
        assert_eq!(inventory.skipped, 3);
    }

    #[test]
    fn rejects_untrusted_addresses_bad_status_and_oversized_documents() {
        for address in [
            "127.0.0.1",
            "192.168.1.2",
            "100.128.0.1",
            "::1",
            "fd00::1",
            "example.com",
        ] {
            let raw = serde_json::to_vec(
                &json!({"BackendState":"Running","Self":{"TailscaleIPs":[address]}}),
            )
            .unwrap();
            assert_eq!(
                parse_inventory(&raw).err().as_deref(),
                Some("tailnet_discovery_status_invalid")
            );
        }
        assert_eq!(
            parse_inventory(br#"{"BackendState":"Stopped"}"#)
                .err()
                .as_deref(),
            Some("tailnet_discovery_stopped")
        );
        assert_eq!(
            parse_inventory(br#"{"BackendState":"Running"}"#)
                .err()
                .as_deref(),
            Some("tailnet_discovery_status_invalid")
        );
        assert_eq!(
            parse_inventory(&vec![b' '; STATUS_LIMIT + 1])
                .err()
                .as_deref(),
            Some("tailnet_discovery_status_too_large")
        );
    }

    #[test]
    fn caps_probes_with_visible_limited_and_skipped_counts() {
        // Synthetic fixture addresses exercise Tailnet admission; no probes are sent.
        let peers: BTreeMap<_,_> = (2..=90).map(|index| (index.to_string(), json!({"HostName":"device","Online":true,"TailscaleIPs":[format!("100.64.1.{index}")]}))).collect();
        // The synthetic fixture includes one self node in the same admitted range.
        let raw = serde_json::to_vec(
            &json!({"BackendState":"Running","Self":{"TailscaleIPs":["100.64.1.1"]},"Peer":peers}),
        )
        .unwrap();
        let inventory = parse_inventory(&raw).unwrap();
        assert!(inventory.limited);
        assert_eq!(inventory.candidates.len(), 64);
        assert_eq!(inventory.peer_count, 90);
        assert_eq!(inventory.skipped, 26);
    }
}
