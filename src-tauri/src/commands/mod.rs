//! Tauri command modules, grouped by responsibility.
//!
//! Extracted from `lib.rs` in the file-structure refactor - the command
//! bodies are moved verbatim; only module paths in `use` statements and
//! `pub(crate)` visibility changed.

pub mod client_config;
pub mod diagnostics;
pub mod dsh;
pub mod dsh_market;
pub mod dsh_onboarding;
pub mod launcher;
pub mod mcp;
pub(crate) mod model_discovery;
pub mod prompt;
pub mod provider;
pub mod provider_screening;
pub mod proxy;
pub mod settings;
pub mod supplier_catalog;
pub mod supplier_import;
pub mod suppliers;
pub(crate) mod tailnet_publication;
pub mod usage;
pub mod workflow_registry;

#[cfg(test)]
mod mirror_loading_acceptance;

#[cfg(test)]
mod acl_tests {
    //! Native invoke is gated twice: the build-time `AppManifest` in
    //! `build.rs` and the `allow-*` grants in `capabilities/default.json`.
    //! A command registered in `lib.rs` but missing from either list rejects
    //! at runtime while the browser demo keeps working — keep all three
    //! lists in lockstep.

    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;

    fn read(relative: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
    }

    fn registered_commands() -> BTreeSet<String> {
        let source = read("src/lib.rs");
        let marker = "tauri::generate_handler![";
        let start = source.find(marker).expect("generate_handler list") + marker.len();
        let end = source[start..].find("];").expect("handler list end") + start;
        source[start..end]
            .split(',')
            .filter_map(|entry| {
                let entry = entry.trim();
                (!entry.is_empty()).then(|| {
                    entry
                        .rsplit("::")
                        .next()
                        .expect("handler path")
                        .trim()
                        .to_string()
                })
            })
            .collect()
    }

    fn manifest_commands() -> BTreeSet<String> {
        let source = read("build.rs");
        let marker = ".commands(&[";
        let start = source.find(marker).expect("AppManifest command list") + marker.len();
        let end = source[start..].find("])").expect("AppManifest list end") + start;
        source[start..end]
            .split(',')
            .filter_map(|entry| {
                let entry = entry.trim();
                entry
                    .strip_prefix('"')
                    .and_then(|entry| entry.strip_suffix('"'))
                    .map(str::to_string)
            })
            .collect()
    }

    fn capability_commands() -> BTreeSet<String> {
        let source = read("capabilities/default.json");
        let document: serde_json::Value =
            serde_json::from_str(&source).expect("capabilities/default.json is valid JSON");
        document["permissions"]
            .as_array()
            .expect("permissions array")
            .iter()
            .filter_map(|permission| permission.as_str())
            .filter_map(|permission| permission.strip_prefix("allow-"))
            .map(|command| command.replace('-', "_"))
            .collect()
    }

    #[test]
    fn registered_commands_match_the_acl_manifest_and_capability_grants() {
        let registered = registered_commands();
        let manifest = manifest_commands();
        let capability = capability_commands();

        let missing_from_manifest: Vec<_> = registered.difference(&manifest).cloned().collect();
        let missing_from_capability: Vec<_> = registered.difference(&capability).cloned().collect();
        let stale_manifest: Vec<_> = manifest.difference(&registered).cloned().collect();
        let stale_capability: Vec<_> = capability.difference(&manifest).cloned().collect();

        assert!(
            missing_from_manifest.is_empty(),
            "registered in lib.rs but missing from the build.rs AppManifest \
             (native invoke would be rejected): {missing_from_manifest:?}"
        );
        assert!(
            missing_from_capability.is_empty(),
            "registered in lib.rs but not granted in capabilities/default.json: \
             {missing_from_capability:?}"
        );
        assert!(
            stale_manifest.is_empty(),
            "AppManifest lists commands that are not registered: {stale_manifest:?}"
        );
        assert!(
            stale_capability.is_empty(),
            "capabilities grant commands missing from the AppManifest: {stale_capability:?}"
        );
    }
}

pub(crate) mod tailnet_discovery;

pub(crate) mod prompt_workspace;
