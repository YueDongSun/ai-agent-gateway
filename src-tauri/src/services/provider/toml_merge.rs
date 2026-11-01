//! Comment-preserving TOML merge for user-owned client configuration.
//!
//! Codex's `config.toml` is written by hand as often as by tool. The previous
//! writer rendered the whole file from a `format!` template, so every provider
//! switch destroyed the user's comments and any unrelated key they had set —
//! ordinary data loss on an ordinary operation. Magpie solves this by splicing
//! raw bytes against a comment-blanked copy of the original
//! (`internal/edit/toml.go`); the Rust equivalent is `toml_edit`, which parses
//! into a document that *retains* formatting and comments, so assigning one
//! value and re-rendering touches only that value.
//!
//! The invariant this module exists to hold: the output must be
//! byte-identical to the input everywhere except the keys AAG was asked to
//! set. `round_trips_untouched` asserts exactly that.
//!
//! Failure is never silent. A file that does not parse is reported as an
//! error, not rewritten — a config AAG cannot understand must reach the user
//! unchanged, not lose its contents to a best-effort guess.

use toml_edit::{value, DocumentMut};

use crate::error::AppError;

/// A TOML value AAG wants present in a user's config.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone)]
pub(crate) enum Toml_value {
    Str(String),
    Bool(bool),
    /// A nested table, e.g. `base_url` / `wire_api` under
    /// `[model_providers.aag]`. Entries are written in the order given.
    /// Callers that manage a table whole build it through `Toml_table`
    /// directly; this variant carries an inline table as a *value*.
    #[cfg(test)]
    Table(Vec<(String, Toml_value)>),
}

impl Toml_value {
    fn into_item(self) -> toml_edit::Item {
        match self {
            Toml_value::Str(text) => value(text),
            Toml_value::Bool(flag) => value(flag),
            #[cfg(test)]
            Toml_value::Table(entries) => {
                let mut table = toml_edit::Table::new();
                for (key, entry) in entries {
                    table.insert(&key, entry.into_item());
                }
                toml_edit::Item::Table(table)
            }
        }
    }
}

/// A top-level key and the value AAG wants it to hold.
#[allow(non_camel_case_types)]
pub(crate) type Toml_top_entry = (String, Toml_value);
/// One managed table: its dotted path, and the keys AAG sets inside it.
#[allow(non_camel_case_types)]
pub(crate) type Toml_table = (String, Vec<(String, Toml_value)>);

/// Set top-level keys and whole tables, preserving everything else.
///
/// `existing` is the file's current bytes (empty when the file is absent).
/// Returns the merged bytes; the caller writes them through
/// `config_writes::write_files`, so validation, backup, atomicity and
/// rollback all stay on the shared path.
///
/// The rendered output is re-parsed before it is returned, so a value AAG
/// cannot spell (a control character in a base URL, say) is refused here
/// rather than leaving a file Codex cannot read.
pub(crate) fn merge(
    existing: &[u8],
    top: &[Toml_top_entry],
    tables: &[Toml_table],
) -> Result<Vec<u8>, AppError> {
    let text = std::str::from_utf8(existing)
        .map_err(|_| AppError::Config("client_config_toml_not_utf8".into()))?;
    // A BOM survives the round trip below only if it is carried explicitly;
    // stripping and restoring keeps the user's file byte-identical.
    let (bom, body) = match text.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", text),
    };

    let mut doc = if body.trim().is_empty() {
        DocumentMut::new()
    } else {
        body.parse::<DocumentMut>()
            .map_err(|e| AppError::Config(format!("client_config_toml_invalid: {e}")))?
    };

    for (key, entry) in top {
        doc[key] = entry.clone().into_item();
    }
    for (path, entries) in tables {
        let table = table_at(&mut doc, path)?;
        // AAG owns this table wholly: drop any key the previous write left
        // that the new value set does not mention, or a stale key would keep
        // steering the agent. Keys outside the managed tables are untouched.
        let wanted: Vec<String> = entries.iter().map(|(key, _)| key.clone()).collect();
        let stale: Vec<String> = table
            .iter()
            .map(|(key, _)| key.to_string())
            .filter(|key| !wanted.contains(key))
            .collect();
        for key in stale {
            table.remove(&key);
        }
        for (key, entry) in entries {
            let item = entry.clone().into_item();
            // Overwrite only the value of a key that already exists, so the
            // key's decor survives — its indentation and any trailing
            // comment. `Table::insert` would replace the whole entry and take
            // `# why I set this` with it. This is the same preservation
            // Magpie gets from rewriting only a key's value span
            // (`internal/edit/toml.go:180-217`).
            match table.get_key_value_mut(key) {
                Some((_key, existing)) => {
                    // An existing scalar becomes the new value; a stored array
                    // or inline table keeps its own shape, which AAG never
                    // writes into a managed key, so replacing the item is safe.
                    match (existing.as_value_mut(), item.as_value()) {
                        (Some(slot), Some(new_value)) => {
                            // The trailing comment lives in the value's own
                            // decor (what follows the closing quote on the
                            // line), not the key's. Carry it across, or
                            // `base_url = "..."  # note` loses `# note`.
                            let mut replacement = new_value.clone();
                            *replacement.decor_mut() = slot.decor().clone();
                            *slot = replacement;
                        }
                        _ => *existing = item,
                    }
                }
                None => {
                    table.insert(key, item);
                }
            }
        }
    }

    let rendered = doc.to_string();
    // Validate the exact bytes AAG is about to write. `toml_edit` can emit a
    // document its own parser rejects when a value contains a byte the writer
    // escapes differently than the parser reads; refusing here is what keeps
    // that from reaching the user's config.
    if rendered.parse::<DocumentMut>().is_err() {
        return Err(AppError::Config(
            "client_config_toml_merge_produced_invalid_document".into(),
        ));
    }
    Ok(format!("{bom}{rendered}").into_bytes())
}

/// Resolve a dotted table path to a mutable table, creating intermediate
/// tables only where the document has none.
///
/// Walking segment by segment matters: `doc["a.b"]` on a document that
/// already spells `[a]` then `[a.b]` as headers creates a *dotted-key* leaf
/// under `a`, leaving the real `[a.b]` table intact beside it — two
/// definitions of the same table, which a TOML parser rejects. Descending
/// through the existing tables reaches the table the user actually wrote, so
/// its comments and header survive.
///
/// An intermediate table is created implicit (no header of its own): it exists
/// only to hold the child, and `toml_edit` renders it as part of the child's
/// path. A terminal table is always made explicit so the next merge finds it.
fn table_at<'a>(
    doc: &'a mut DocumentMut,
    path: &str,
) -> Result<&'a mut toml_edit::Table, AppError> {
    let mut parts = path.split('.').peekable();
    // The first segment is a real top-level table in the document.
    let mut current = doc.as_table_mut();
    while let Some(part) = parts.next() {
        let last = parts.peek().is_none();
        // An intermediate table needs no header of its own; the terminal one
        // does, or the next merge would not find it.
        let default = || {
            let mut table = toml_edit::Table::new();
            table.set_implicit(!last);
            toml_edit::Item::Table(table)
        };
        let item = current.entry(part).or_insert_with(default);
        current = item.as_table_mut().ok_or_else(|| {
            AppError::Config(format!(
                "client_config_toml_table_conflict: {path} is not a table"
            ))
        })?;
    }
    Ok(current)
}

/// Shorthand for the string tables the Codex provider block is made of.
#[cfg(test)]
pub(crate) fn str_table(entries: &[(&str, &str)]) -> Toml_value {
    Toml_value::Table(
        entries
            .iter()
            .map(|(key, entry)| ((*key).to_string(), Toml_value::Str((*entry).to_string())))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant that motivates this module: a document AAG does not touch
    /// must come back byte-identical, comments included.
    #[test]
    fn round_trips_untouched() {
        let source = "# my codex config\n\
                    \n\
                    # keep me\n\
                    [model_providers.other]\n\
                    base_url = \"https://example.com\"  # trailing note\n\
                    \n\
                    # a full-line comment\n\
                    [profiles.work]\n\
                    model = \"gpt-5\"\n";
        let out = merge(source.as_bytes(), &[], &[]).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            source,
            "an empty merge must not reformat the document"
        );
    }

    /// The defect being fixed: switching providers previously rewrote the file
    /// from a template and took the user's comments with it.
    #[test]
    fn setting_one_table_key_preserves_user_comments_and_keys() {
        let source = "# my codex config\n\
                    \n\
                    approval_policy = \"on-request\"\n\
                    \n\
                    [model_providers.other]\n\
                    base_url = \"https://example.com\"  # trailing note\n";
        let out = merge(
            source.as_bytes(),
            &[],
            &[(
                "model_providers.aag".to_string(),
                vec![(
                    "base_url".to_string(),
                    Toml_value::Str("http://127.0.0.1:8080".into()),
                )],
            )],
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("# my codex config"),
            "the user's leading comment must survive: {text}"
        );
        assert!(
            text.contains("# trailing note"),
            "a trailing comment on an untouched key must survive: {text}"
        );
        assert!(
            text.contains("approval_policy = \"on-request\""),
            "an unrelated top-level key must survive: {text}"
        );
        assert!(
            text.contains("[model_providers.other]"),
            "an unrelated table must survive: {text}"
        );
        assert!(
            text.contains("http://127.0.0.1:8080"),
            "the managed key must be written: {text}"
        );
    }

    #[test]
    fn an_existing_managed_value_is_replaced_in_place() {
        let source = "[model_providers.aag]\nbase_url = \"http://old:1\"  # keep\n";
        let out = merge(
            source.as_bytes(),
            &[],
            &[(
                "model_providers.aag".to_string(),
                vec![(
                    "base_url".to_string(),
                    Toml_value::Str("http://new:2".into()),
                )],
            )],
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("http://new:2"));
        assert!(!text.contains("http://old:1"), "the old value must be gone");
        assert!(text.contains("# keep"), "its comment must remain: {text}");
    }

    #[test]
    fn an_empty_document_gains_only_the_managed_keys() {
        let out = merge(
            b"",
            &[("model".to_string(), Toml_value::Str("gpt-5".into()))],
            &[],
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("model = \"gpt-5\""));
        assert_eq!(text.parse::<DocumentMut>().unwrap().len(), 1);
    }

    /// A control character in a value must reach the file in a form a TOML
    /// parser reads back unchanged. The previous hand-rolled escaper handled
    /// only `\` and `"`, so a base URL carrying, say, a DEL was written raw
    /// and corrupted the document; `toml_edit` escapes it to ``, which
    /// round-trips. This test pins that safety rather than asserting refusal.
    #[test]
    fn a_control_character_is_escaped_so_the_file_stays_parseable() {
        let source = "[model_providers.aag]\nbase_url = \"http://ok\"\n";
        let out = merge(
            source.as_bytes(),
            &[],
            &[(
                "model_providers.aag".to_string(),
                vec![(
                    "base_url".to_string(),
                    Toml_value::Str("bad\u{7f}value".into()),
                )],
            )],
        )
        .expect("a control character must not fail the merge");
        let text = String::from_utf8(out).unwrap();
        // The emitted document must parse, and reading it back must yield the
        // exact value that was set — not a mangled or truncated one.
        let reparsed = text
            .parse::<DocumentMut>()
            .expect("the emitted document must be valid TOML");
        let read_back = reparsed
            .get("model_providers")
            .and_then(|p| p.get("aag"))
            .and_then(|a| a.get("base_url"))
            .and_then(|v| v.as_str())
            .expect("base_url must be readable");
        assert_eq!(read_back, "bad\u{7f}value");
    }

    /// A config AAG cannot parse is reported, never rewritten.
    #[test]
    fn an_unparseable_config_is_an_error_not_a_rewrite() {
        let broken = b"this is not [ toml at all\n= = =\n";
        let result = merge(
            broken,
            &[("model".to_string(), Toml_value::Str("x".into()))],
            &[],
        );
        assert!(
            result.is_err(),
            "an unparseable config must not be rewritten"
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("client_config_toml_invalid"));
    }

    #[test]
    fn non_utf8_input_is_refused() {
        assert!(merge(&[0xff, 0xfe], &[], &[]).is_err());
    }

    #[test]
    fn a_byte_order_mark_survives() {
        let source = "\u{feff}# kept\nmodel = \"gpt-5\"\n";
        let out = merge(source.as_bytes(), &[], &[]).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), source);
    }

    /// Table replacement is what the Codex mirror relies on: AAG owns
    /// `[model_providers.aag]` wholly, and must not inherit a stale key the
    /// previous write left behind.
    #[test]
    fn a_replaced_table_does_not_keep_stale_keys() {
        let source =
            "[model_providers.aag]\nbase_url = \"http://old:1\"\nwire_api = \"responses\"\n";
        let out = merge(
            source.as_bytes(),
            &[],
            &[(
                "model_providers.aag".to_string(),
                vec![(
                    "base_url".to_string(),
                    Toml_value::Str("http://new:2".into()),
                )],
            )],
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            !text.contains("wire_api"),
            "a stale managed key must be dropped: {text}"
        );
    }

    #[test]
    fn str_table_builds_an_ordered_nested_table() {
        let table = str_table(&[("base_url", "http://x"), ("wire_api", "responses")]);
        let Toml_value::Table(entries) = table else {
            panic!("expected a table");
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "base_url");
    }

    /// The dotted-path traversal must reach the table the user actually
    /// wrote, not create a second definition of it beside the original.
    #[test]
    fn a_nested_header_table_is_updated_not_duplicated() {
        let source = "# mine\n[model_providers.aag]\nbase_url = \"http://old:1\"\n";
        let out = merge(
            source.as_bytes(),
            &[],
            &[(
                "model_providers.aag".to_string(),
                vec![(
                    "base_url".to_string(),
                    Toml_value::Str("http://new:2".into()),
                )],
            )],
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            text.matches("[model_providers.aag]").count()
                + text.matches("[\"model_providers.aag\"]").count(),
            1,
            "the table must be updated in place, not redefined: {text}"
        );
        assert!(text.contains("http://new:2"));
        assert!(
            text.contains("# mine"),
            "the header comment must survive: {text}"
        );
    }

    /// A table AAG has never written is created with its own header, so the
    /// next merge finds it and Codex can read it.
    #[test]
    fn a_missing_table_is_created_with_a_readable_header() {
        let out = merge(
            b"model = \"gpt-5\"\n",
            &[],
            &[(
                "profiles.aag".to_string(),
                vec![("model".to_string(), Toml_value::Str("gpt-5".into()))],
            )],
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        let reparsed = text.parse::<DocumentMut>().expect("must be valid TOML");
        let profiles = reparsed
            .get("profiles")
            .and_then(|p| p.get("aag"))
            .and_then(|a| a.get("model"))
            .and_then(|v| v.as_str())
            .expect("profiles.aag.model must be readable");
        assert_eq!(profiles, "gpt-5");
    }

    /// A path that runs through a non-table must be reported, not silently
    /// overwritten — the user has a key where AAG wants a table.
    #[test]
    fn a_path_through_a_scalar_is_refused() {
        let source = "model_providers = \"just-a-string\"\n";
        let result = merge(
            source.as_bytes(),
            &[],
            &[(
                "model_providers.aag".to_string(),
                vec![("base_url".to_string(), Toml_value::Str("http://x".into()))],
            )],
        );
        assert!(
            result.is_err(),
            "a scalar where a table is needed must be refused"
        );
        assert!(result.unwrap_err().to_string().contains("table_conflict"));
    }
}
