//! Minimal span-splicing JSONC patcher for the auto-switch write paths.
//!
//! The auto-switch writers merge one provider into a client-owned JSONC
//! document. Round-tripping that document through `serde_json::Value` drops
//! the user's comments, key order, indentation and line endings, so this
//! module locates the spans the adapters own in the raw text and splices new
//! values into them. Every byte outside an edited span is preserved verbatim.
//!
//! Exactly three edits are supported:
//!   - replace or insert one top-level member (the `model` key);
//!   - replace or insert one member of the top-level `provider` object,
//!     keyed by provider id (the entry is machine-managed, so its value may
//!     be replaced wholesale);
//!   - merge a flat object into an existing object member (Copilot's `env`:
//!     overwrite present keys, insert missing ones).
//!
//! [`JsoncEditor::finish`] re-parses the patched text (comments stripped,
//! `serde_json`) and compares it with the intended merged value. Any
//! mismatch is an error, so callers fall back to a full re-serialization
//! instead of writing a document the adapters did not intend.

use crate::error::AppError;
use serde_json::{Map, Value};

/// Stable internal error code: the raw document could not be patched safely,
/// or the patched text did not re-parse to the intended merged value.
const PATCH_UNSAFE: &str = "AAG_JSONC_PATCH_UNSAFE";

fn unsafe_patch() -> AppError {
    AppError::Config(PATCH_UNSAFE.into())
}

/// Byte span in the raw document. Spans always start and end on ASCII
/// delimiters, so slicing at them is UTF-8 safe.
#[derive(Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
}

/// One object member: decoded key, key-token start (for indentation
/// inference), and its value node.
struct MemberNode {
    key: String,
    key_start: usize,
    value: Node,
}

struct ObjectNode {
    open: usize,
    close: usize,
    members: Vec<MemberNode>,
}

enum Node {
    Object(ObjectNode),
    /// Arrays and scalars: only the span is needed.
    Other(Span),
}

impl Node {
    fn span(&self) -> Span {
        match self {
            Node::Object(object) => Span {
                start: object.open,
                end: object.close + 1,
            },
            Node::Other(span) => *span,
        }
    }
}

/// A parsed JSONC document plus the formatting conventions used to render
/// inserted members.
struct Document {
    root: ObjectNode,
    /// One indentation level, inferred from the shortest structural indent
    /// in the document (`"  "` when there is none).
    unit: String,
    /// Line ending used for inserted lines (matches the document).
    newline: &'static str,
}

impl Document {
    fn parse(raw: &str) -> Result<Document, AppError> {
        let root = Parser::parse_document(raw)?;
        let comments = comment_ranges(raw);
        Ok(Document {
            root,
            unit: detect_indent_unit(raw, &comments),
            newline: if first_structural_newline_is_crlf(raw, &comments) {
                "\r\n"
            } else {
                "\n"
            },
        })
    }
}

/// Byte spans of `//` and `/* */` comments outside string literals. Used to
/// keep formatting inference from reading comment prose as structure.
fn comment_ranges(raw: &str) -> Vec<Span> {
    let bytes = raw.as_bytes();
    let mut ranges = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        match bytes[pos] {
            b'"' => {
                pos += 1;
                while pos < bytes.len() {
                    match bytes[pos] {
                        b'\\' => pos += 2,
                        b'"' => {
                            pos += 1;
                            break;
                        }
                        _ => pos += 1,
                    }
                }
            }
            b'/' if bytes.get(pos + 1) == Some(&b'/') => {
                let start = pos;
                pos += 2;
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    pos += 1;
                }
                ranges.push(Span { start, end: pos });
            }
            b'/' if bytes.get(pos + 1) == Some(&b'*') => {
                let start = pos;
                pos += 2;
                loop {
                    if pos + 1 >= bytes.len() {
                        pos = bytes.len();
                        break;
                    }
                    if bytes[pos] == b'*' && bytes[pos + 1] == b'/' {
                        pos += 2;
                        break;
                    }
                    pos += 1;
                }
                ranges.push(Span { start, end: pos });
            }
            _ => pos += 1,
        }
    }
    ranges
}

/// Line ending of the first newline that is not inside a block comment (a
/// stray `\r\n` in comment prose must not decide the document's line ending).
fn first_structural_newline_is_crlf(raw: &str, comments: &[Span]) -> bool {
    let bytes = raw.as_bytes();
    for (pos, byte) in bytes.iter().enumerate() {
        if *byte == b'\n'
            && !comments
                .iter()
                .any(|span| pos >= span.start && pos < span.end)
        {
            return pos > 0 && bytes[pos - 1] == b'\r';
        }
    }
    false
}

/// Recursive-descent scanner that understands just enough JSONC (strings
/// with escapes, `//` line comments, `/* */` block comments) to record
/// object-member spans. Values the patcher never edits are still parsed so
/// the scanner cannot mistake data for structure.
struct Parser<'a> {
    raw: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn parse_document(raw: &'a str) -> Result<ObjectNode, AppError> {
        let mut parser = Parser {
            raw,
            bytes: raw.as_bytes(),
            pos: 0,
        };
        parser.skip_trivia()?;
        if parser.peek() != Some(b'{') {
            return Err(unsafe_patch());
        }
        let root = parser.parse_object()?;
        parser.skip_trivia()?;
        if parser.pos != parser.bytes.len() {
            return Err(unsafe_patch());
        }
        Ok(root)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn expect(&mut self, expected: u8) -> Result<(), AppError> {
        if self.peek() == Some(expected) {
            self.pos += 1;
            Ok(())
        } else {
            Err(unsafe_patch())
        }
    }

    fn expect_literal(&mut self, literal: &[u8]) -> Result<(), AppError> {
        if self.bytes.get(self.pos..self.pos + literal.len()) == Some(literal) {
            self.pos += literal.len();
            Ok(())
        } else {
            Err(unsafe_patch())
        }
    }

    /// Skip whitespace, `//` line comments and `/* */` block comments.
    fn skip_trivia(&mut self) -> Result<(), AppError> {
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') => self.pos += 1,
                Some(b'/') => match self.bytes.get(self.pos + 1) {
                    Some(b'/') => {
                        self.pos += 2;
                        while let Some(byte) = self.peek() {
                            if byte == b'\n' {
                                break;
                            }
                            self.pos += 1;
                        }
                    }
                    Some(b'*') => {
                        self.pos += 2;
                        loop {
                            match self.peek() {
                                Some(b'*') if self.bytes.get(self.pos + 1) == Some(&b'/') => {
                                    self.pos += 2;
                                    break;
                                }
                                Some(_) => self.pos += 1,
                                None => return Err(unsafe_patch()),
                            }
                        }
                    }
                    _ => return Err(unsafe_patch()),
                },
                _ => return Ok(()),
            }
        }
    }

    fn parse_string_span(&mut self) -> Result<Span, AppError> {
        let start = self.pos;
        self.expect(b'"')?;
        loop {
            match self.bump() {
                // Skipping one byte after a backslash is enough to find the
                // terminating quote (the skipped byte cannot be the closer).
                Some(b'\\') => {
                    self.bump();
                }
                Some(b'"') => break,
                Some(_) => {}
                None => return Err(unsafe_patch()),
            }
        }
        Ok(Span {
            start,
            end: self.pos,
        })
    }

    fn parse_key(&mut self) -> Result<(String, usize), AppError> {
        let span = self.parse_string_span()?;
        let key: String =
            serde_json::from_str(&self.raw[span.start..span.end]).map_err(|_| unsafe_patch())?;
        Ok((key, span.start))
    }

    fn parse_object(&mut self) -> Result<ObjectNode, AppError> {
        let open = self.pos;
        self.expect(b'{')?;
        let mut members = Vec::new();
        self.skip_trivia()?;
        if self.peek() == Some(b'}') {
            let close = self.pos;
            self.pos += 1;
            return Ok(ObjectNode {
                open,
                close,
                members,
            });
        }
        loop {
            self.skip_trivia()?;
            if self.peek() != Some(b'"') {
                return Err(unsafe_patch());
            }
            let (key, key_start) = self.parse_key()?;
            self.skip_trivia()?;
            self.expect(b':')?;
            self.skip_trivia()?;
            let value = self.parse_value()?;
            members.push(MemberNode {
                key,
                key_start,
                value,
            });
            self.skip_trivia()?;
            match self.peek() {
                // A trailing comma is not valid for callers (they parse the
                // document with serde_json today), so it is rejected here.
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    let close = self.pos;
                    self.pos += 1;
                    return Ok(ObjectNode {
                        open,
                        close,
                        members,
                    });
                }
                _ => return Err(unsafe_patch()),
            }
        }
    }

    fn parse_array(&mut self) -> Result<Span, AppError> {
        let start = self.pos;
        self.expect(b'[')?;
        self.skip_trivia()?;
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Span {
                start,
                end: self.pos,
            });
        }
        loop {
            self.skip_trivia()?;
            self.parse_value()?;
            self.skip_trivia()?;
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Span {
                        start,
                        end: self.pos,
                    });
                }
                _ => return Err(unsafe_patch()),
            }
        }
    }

    fn parse_number(&mut self) -> Result<Node, AppError> {
        let start = self.pos;
        while let Some(byte) = self.peek() {
            match byte {
                b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9' => self.pos += 1,
                _ => break,
            }
        }
        if self.pos == start {
            return Err(unsafe_patch());
        }
        // Reject token shapes serde_json would not accept (`1.2.3`, `1e+`),
        // so the scanner and the verifying parser agree on what a number is.
        serde_json::from_str::<Value>(&self.raw[start..self.pos]).map_err(|_| unsafe_patch())?;
        Ok(Node::Other(Span {
            start,
            end: self.pos,
        }))
    }

    fn parse_value(&mut self) -> Result<Node, AppError> {
        let start = self.pos;
        match self.peek() {
            Some(b'{') => Ok(Node::Object(self.parse_object()?)),
            Some(b'[') => Ok(Node::Other(self.parse_array()?)),
            Some(b'"') => Ok(Node::Other(self.parse_string_span()?)),
            Some(b't') => {
                self.expect_literal(b"true")?;
                Ok(Node::Other(Span {
                    start,
                    end: self.pos,
                }))
            }
            Some(b'f') => {
                self.expect_literal(b"false")?;
                Ok(Node::Other(Span {
                    start,
                    end: self.pos,
                }))
            }
            Some(b'n') => {
                self.expect_literal(b"null")?;
                Ok(Node::Other(Span {
                    start,
                    end: self.pos,
                }))
            }
            Some(b'-') | Some(b'0'..=b'9') => self.parse_number(),
            _ => Err(unsafe_patch()),
        }
    }
}

/// Last member with `key` wins, matching `serde_json`'s duplicate-key
/// behavior for the parsed value the adapters merge as the expected result.
fn find_member<'a>(object: &'a ObjectNode, key: &str) -> Option<&'a MemberNode> {
    object.members.iter().rev().find(|member| member.key == key)
}

/// Whitespace between the start of `pos`'s line and `pos`, or `None` when
/// the token does not start its line (a one-line / inline object).
fn line_indent(raw: &str, pos: usize) -> Option<&str> {
    let line_start = raw[..pos].rfind('\n').map(|index| index + 1).unwrap_or(0);
    let prefix = &raw[line_start..pos];
    prefix
        .chars()
        .all(|character| character == ' ' || character == '\t')
        .then_some(prefix)
}

/// Infer one indentation level: the shortest indent that prefixes a
/// structural line (member key, brace, bracket). Comment lines are excluded
/// because their indentation is prose, not structure.
fn detect_indent_unit(raw: &str, comments: &[Span]) -> String {
    let mut best: Option<&str> = None;
    let mut line_start = 0;
    for line in raw.split('\n') {
        let indent_len = line
            .bytes()
            .take_while(|byte| *byte == b' ' || *byte == b'\t')
            .count();
        let first_pos = line_start + indent_len;
        let structural = indent_len > 0
            && indent_len < line.len()
            && matches!(
                line.as_bytes()[indent_len],
                b'"' | b'{' | b'}' | b'[' | b']'
            )
            && !comments
                .iter()
                .any(|span| first_pos >= span.start && first_pos < span.end);
        if structural {
            let indent = &line[..indent_len];
            if best.map_or(true, |current| indent.len() < current.len()) {
                best = Some(indent);
            }
        }
        line_start += line.len() + 1;
    }
    best.map(str::to_string)
        .unwrap_or_else(|| String::from("  "))
}

fn splice(raw: &str, start: usize, end: usize, replacement: &str) -> Result<String, AppError> {
    if start > end || end > raw.len() || !raw.is_char_boundary(start) || !raw.is_char_boundary(end)
    {
        return Err(unsafe_patch());
    }
    let mut out = String::with_capacity(raw.len() - (end - start) + replacement.len());
    out.push_str(&raw[..start]);
    out.push_str(replacement);
    out.push_str(&raw[end..]);
    Ok(out)
}

/// Render `value` as pretty JSONC at `base` indentation. Nested lines are
/// indented by `unit`; the closing brace/bracket sits at `base`.
fn render_value(value: &Value, unit: &str, base: &str, newline: &str) -> Result<String, AppError> {
    let inner = format!("{base}{unit}");
    match value {
        Value::Object(map) if !map.is_empty() => {
            let mut out = String::from("{");
            out.push_str(newline);
            for (index, (key, child)) in map.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                    out.push_str(newline);
                }
                out.push_str(&inner);
                out.push_str(&serde_json::to_string(key).map_err(|_| unsafe_patch())?);
                out.push_str(": ");
                out.push_str(&render_value(child, unit, &inner, newline)?);
            }
            out.push_str(newline);
            out.push_str(base);
            out.push('}');
            Ok(out)
        }
        Value::Array(items) if !items.is_empty() => {
            let mut out = String::from("[");
            out.push_str(newline);
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                    out.push_str(newline);
                }
                out.push_str(&inner);
                out.push_str(&render_value(item, unit, &inner, newline)?);
            }
            out.push_str(newline);
            out.push_str(base);
            out.push(']');
            Ok(out)
        }
        _ => serde_json::to_string(value).map_err(|_| unsafe_patch()),
    }
}

/// Replace the value span of an existing member.
fn replace_value(
    raw: &str,
    doc: &Document,
    member: &MemberNode,
    value: &Value,
) -> Result<String, AppError> {
    let span = member.value.span();
    let rendered = match line_indent(raw, member.key_start) {
        Some(indent) => render_value(value, &doc.unit, indent, doc.newline)?,
        None => serde_json::to_string(value).map_err(|_| unsafe_patch())?,
    };
    splice(raw, span.start, span.end, &rendered)
}

/// True when the rest of `pos`'s line (after optional whitespace) starts a
/// comment, so an appended member would land between a value and its own
/// trailing comment.
fn has_trailing_comment(raw: &str, pos: usize) -> bool {
    let bytes = raw.as_bytes();
    let mut cursor = pos;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b' ' | b'\t' => cursor += 1,
            b'/' if matches!(bytes.get(cursor + 1), Some(b'/') | Some(b'*')) => return true,
            _ => return false,
        }
    }
    false
}

/// Insert one member line into `object`, deriving indentation from its
/// siblings and keeping comma placement valid.
fn insert_member(
    raw: &str,
    doc: &Document,
    object: &ObjectNode,
    key: &str,
    value: &Value,
) -> Result<String, AppError> {
    let key_json = serde_json::to_string(key).map_err(|_| unsafe_patch())?;
    if let Some(first) = object.members.first() {
        let last = object
            .members
            .last()
            .expect("members is non-empty when first exists");
        // A member with a trailing comment on its own line keeps that
        // comment: the new member goes in front of it instead of between the
        // value and the comment that documents it.
        if let Some(last_indent) = line_indent(raw, last.key_start) {
            if has_trailing_comment(raw, last.value.span().end) {
                let rendered = render_value(value, &doc.unit, last_indent, doc.newline)?;
                let insertion = format!(
                    "{key_json}: {rendered},{newline}{last_indent}",
                    newline = doc.newline
                );
                return splice(raw, last.key_start, last.key_start, &insertion);
            }
        }
        let insert_at = last.value.span().end;
        let sibling_indent = line_indent(raw, first.key_start)
            .filter(|_| raw[object.open..first.key_start].contains('\n'));
        let insertion = match sibling_indent {
            Some(indent) => {
                let rendered = render_value(value, &doc.unit, indent, doc.newline)?;
                format!(
                    ",{newline}{indent}{key_json}: {rendered}",
                    newline = doc.newline
                )
            }
            None => {
                let rendered = serde_json::to_string(value).map_err(|_| unsafe_patch())?;
                format!(", {key_json}: {rendered}")
            }
        };
        return splice(raw, insert_at, insert_at, &insertion);
    }

    // Empty object: there is no sibling line to copy.
    let gap = &raw[object.open + 1..object.close];
    let close_indent = line_indent(raw, object.close).filter(|_| gap.contains('\n'));
    let insertion = match close_indent {
        Some(close_indent) => {
            let indent = format!("{close_indent}{}", doc.unit);
            let rendered = render_value(value, &doc.unit, &indent, doc.newline)?;
            format!(
                "{newline}{indent}{key_json}: {rendered}",
                newline = doc.newline
            )
        }
        None => {
            let rendered = serde_json::to_string(value).map_err(|_| unsafe_patch())?;
            format!("{key_json}: {rendered}")
        }
    };
    let insert_at = object.open + 1;
    splice(raw, insert_at, insert_at, &insertion)
}

/// Span-splicing editor over one raw JSONC document.
///
/// Construct it with [`JsoncEditor::new`] (the raw text must parse as a
/// JSONC object), apply the three supported edits, then call
/// [`JsoncEditor::finish`] with the merged value the adapter intends to
/// write. `finish` verifies the patched text before it is returned.
pub struct JsoncEditor {
    text: String,
}

impl JsoncEditor {
    /// Parse `raw` as a JSONC document whose root is an object. The raw text
    /// must also parse through the same comment-stripping + `serde_json`
    /// path the adapters use, so the patcher never accepts a document the
    /// writers would reject.
    pub fn new(raw: &str) -> Result<JsoncEditor, AppError> {
        Document::parse(raw)?;
        let stripped = super::config_editor::strip_jsonc_comments(raw);
        serde_json::from_str::<Value>(&stripped).map_err(|_| unsafe_patch())?;
        Ok(JsoncEditor {
            text: raw.to_string(),
        })
    }

    /// Replace or insert a top-level member.
    pub fn set_top_level(&mut self, key: &str, value: &Value) -> Result<(), AppError> {
        let doc = Document::parse(&self.text)?;
        let updated = match find_member(&doc.root, key) {
            Some(member) => replace_value(&self.text, &doc, member, value)?,
            None => insert_member(&self.text, &doc, &doc.root, key, value)?,
        };
        self.text = updated;
        Ok(())
    }

    /// Replace or insert `provider.<provider_id>`; the top-level `provider`
    /// object is created when the document has none.
    pub fn set_provider_entry(&mut self, provider_id: &str, value: &Value) -> Result<(), AppError> {
        let doc = Document::parse(&self.text)?;
        let updated = match find_member(&doc.root, "provider") {
            Some(member) => match &member.value {
                Node::Object(providers) => match find_member(providers, provider_id) {
                    Some(entry) => replace_value(&self.text, &doc, entry, value)?,
                    None => insert_member(&self.text, &doc, providers, provider_id, value)?,
                },
                Node::Other(_) => return Err(unsafe_patch()),
            },
            None => {
                let mut providers = Map::new();
                providers.insert(provider_id.to_string(), value.clone());
                insert_member(
                    &self.text,
                    &doc,
                    &doc.root,
                    "provider",
                    &Value::Object(providers),
                )?
            }
        };
        self.text = updated;
        Ok(())
    }

    /// Overwrite the keys of `patch` that already exist in the object
    /// member `key`, insert the missing ones, and leave everything else
    /// (including comments inside the object) untouched.
    pub fn merge_object(&mut self, key: &str, patch: &Map<String, Value>) -> Result<(), AppError> {
        for (patch_key, patch_value) in patch {
            self.merge_object_member(key, patch_key, patch_value)?;
        }
        Ok(())
    }

    fn merge_object_member(
        &mut self,
        key: &str,
        patch_key: &str,
        patch_value: &Value,
    ) -> Result<(), AppError> {
        let doc = Document::parse(&self.text)?;
        let updated = match find_member(&doc.root, key) {
            Some(member) => match &member.value {
                Node::Object(target) => match find_member(target, patch_key) {
                    Some(existing) => replace_value(&self.text, &doc, existing, patch_value)?,
                    None => insert_member(&self.text, &doc, target, patch_key, patch_value)?,
                },
                Node::Other(_) => return Err(unsafe_patch()),
            },
            None => {
                let mut object = Map::new();
                object.insert(patch_key.to_string(), patch_value.clone());
                insert_member(&self.text, &doc, &doc.root, key, &Value::Object(object))?
            }
        };
        self.text = updated;
        Ok(())
    }

    /// Re-parse the patched document and require it to equal `expected`
    /// exactly; a splice that quietly produced different data must never be
    /// written.
    pub fn finish(self, expected: &Value) -> Result<String, AppError> {
        let stripped = super::config_editor::strip_jsonc_comments(&self.text);
        let parsed: Value = serde_json::from_str(&stripped).map_err(|_| unsafe_patch())?;
        if &parsed != expected {
            return Err(unsafe_patch());
        }
        Ok(self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object")
    }

    #[test]
    fn replacing_a_provider_entry_edits_only_that_span() {
        let raw = concat!(
            "{\n",
            "  // user comment\n",
            "  \"theme\": \"dark\", /* keep */\n",
            "  \"provider\": {\n",
            "      \"openai\": { \"npm\": \"x\" },\n",
            "      \"aag\": { \"npm\": \"old\" }\n",
            "  }\n",
            "}\n",
        );
        let mut editor = JsoncEditor::new(raw).expect("parses");
        editor
            .set_provider_entry("aag", &json!({ "npm": "new" }))
            .expect("entry replaced");
        let patched = editor
            .finish(&json!({
                "theme": "dark",
                "provider": {
                    "openai": { "npm": "x" },
                    "aag": { "npm": "new" }
                }
            }))
            .expect("verified");
        assert_eq!(
            patched,
            concat!(
                "{\n",
                "  // user comment\n",
                "  \"theme\": \"dark\", /* keep */\n",
                "  \"provider\": {\n",
                "      \"openai\": { \"npm\": \"x\" },\n",
                "      \"aag\": {\n",
                "        \"npm\": \"new\"\n",
                "      }\n",
                "  }\n",
                "}\n",
            )
        );
    }

    #[test]
    fn inserting_a_missing_provider_keeps_sibling_bytes() {
        let raw = concat!(
            "{\n",
            "    \"provider\": {\n",
            "        \"openai\": { \"npm\": \"x\" } // keep me\n",
            "    }\n",
            "}\n",
        );
        let mut editor = JsoncEditor::new(raw).expect("parses");
        editor
            .set_provider_entry("aag", &json!({ "npm": "new" }))
            .expect("entry inserted");
        let patched = editor
            .finish(&json!({
                "provider": { "aag": { "npm": "new" }, "openai": { "npm": "x" } }
            }))
            .expect("verified");
        assert_eq!(
            patched,
            concat!(
                "{\n",
                "    \"provider\": {\n",
                "        \"aag\": {\n",
                "            \"npm\": \"new\"\n",
                "        },\n",
                "        \"openai\": { \"npm\": \"x\" } // keep me\n",
                "    }\n",
                "}\n",
            )
        );
    }

    #[test]
    fn inserting_before_a_trailing_block_comment_keeps_it_with_its_member() {
        let raw = concat!(
            "{\n",
            "  \"provider\": {\n",
            "    \"openai\": { \"npm\": \"x\" } /* endpoint */\n",
            "  }\n",
            "}\n",
        );
        let mut editor = JsoncEditor::new(raw).expect("parses");
        editor
            .set_provider_entry("aag", &json!({ "npm": "new" }))
            .expect("entry inserted");
        let patched = editor
            .finish(&json!({
                "provider": { "aag": { "npm": "new" }, "openai": { "npm": "x" } }
            }))
            .expect("verified");
        assert_eq!(
            patched,
            concat!(
                "{\n",
                "  \"provider\": {\n",
                "    \"aag\": {\n",
                "      \"npm\": \"new\"\n",
                "    },\n",
                "    \"openai\": { \"npm\": \"x\" } /* endpoint */\n",
                "  }\n",
                "}\n",
            )
        );
    }

    #[test]
    fn top_level_member_is_inserted_and_then_replaced() {
        let raw = concat!("{\n", "  \"theme\": \"dark\" // tail\n", "}\n",);
        let mut editor = JsoncEditor::new(raw).expect("parses");
        editor
            .set_top_level("model", &json!("aag/glm-5"))
            .expect("inserted");
        editor
            .set_top_level("model", &json!("aag/glm-6"))
            .expect("replaced");
        let patched = editor
            .finish(&json!({ "model": "aag/glm-6", "theme": "dark" }))
            .expect("verified");
        assert_eq!(
            patched,
            concat!(
                "{\n",
                "  \"model\": \"aag/glm-6\",\n",
                "  \"theme\": \"dark\" // tail\n",
                "}\n",
            )
        );
    }

    #[test]
    fn indent_detection_ignores_lines_inside_block_comments() {
        let raw = concat!(
            "{\n",
            "    /* prose example:\n",
            "  \"quoted\" text inside a comment at two spaces\n",
            "    */\n",
            "    \"a\": 1\n",
            "}\n",
        );
        let mut editor = JsoncEditor::new(raw).expect("parses");
        editor
            .set_top_level("b", &json!({ "k": 1 }))
            .expect("inserted");
        let patched = editor
            .finish(&json!({ "a": 1, "b": { "k": 1 } }))
            .expect("verified");
        assert_eq!(
            patched,
            concat!(
                "{\n",
                "    /* prose example:\n",
                "  \"quoted\" text inside a comment at two spaces\n",
                "    */\n",
                "    \"a\": 1,\n",
                "    \"b\": {\n",
                "        \"k\": 1\n",
                "    }\n",
                "}\n",
            )
        );
    }

    #[test]
    fn line_ending_detection_ignores_crlf_inside_block_comments() {
        let raw = "{\n  /* one line\r\n  still comment */\n  \"a\": 1\n}\n";
        let mut editor = JsoncEditor::new(raw).expect("parses");
        editor.set_top_level("b", &json!(2)).expect("inserted");
        let patched = editor.finish(&json!({ "a": 1, "b": 2 })).expect("verified");
        assert!(
            patched.contains("\n  \"b\": 2"),
            "inserted line must use the document's structural LF ending: {patched:?}"
        );
        assert!(!patched.contains("\r\n  \"b\""));
    }

    #[test]
    fn numbers_with_invalid_syntax_are_rejected() {
        for raw in [
            r#"{"a": 1.2.3}"#,
            r#"{"a": 1e+}"#,
            r#"{"a": --5}"#,
            r#"{"a": 01}"#,
        ] {
            assert!(JsoncEditor::new(raw).is_err(), "must reject {raw:?}");
        }
    }

    #[test]
    fn merging_an_object_overwrites_present_keys_and_inserts_missing_ones() {
        let raw = concat!(
            "{\n",
            "  \"env\": {\n",
            "    \"COPILOT_PROVIDER_BASE_URL\": \"https://old.invalid\", // keep\n",
            "    \"OTHER\": \"value\"\n",
            "  },\n",
            "  \"theme\": \"dark\"\n",
            "}\n",
        );
        let patch = map(json!({
            "COPILOT_PROVIDER_BASE_URL": "https://example.invalid/v1",
            "COPILOT_PROVIDER_API_KEY": "sk-secret"
        }));
        let mut editor = JsoncEditor::new(raw).expect("parses");
        editor.merge_object("env", &patch).expect("env merged");
        let patched = editor
            .finish(&json!({
                "env": {
                    "COPILOT_PROVIDER_BASE_URL": "https://example.invalid/v1",
                    "OTHER": "value",
                    "COPILOT_PROVIDER_API_KEY": "sk-secret"
                },
                "theme": "dark"
            }))
            .expect("verified");
        assert_eq!(
            patched,
            concat!(
                "{\n",
                "  \"env\": {\n",
                "    \"COPILOT_PROVIDER_BASE_URL\": \"https://example.invalid/v1\", // keep\n",
                "    \"OTHER\": \"value\",\n",
                "    \"COPILOT_PROVIDER_API_KEY\": \"sk-secret\"\n",
                "  },\n",
                "  \"theme\": \"dark\"\n",
                "}\n",
            )
        );
    }

    #[test]
    fn comment_markers_inside_string_literals_are_data() {
        let raw = concat!(
            "{\n",
            "  \"note\": \"// not a comment\",\n",
            "  \"url\": \"https://example.invalid/*x*/\"\n",
            "}\n",
        );
        let mut editor = JsoncEditor::new(raw).expect("parses");
        editor
            .set_top_level("model", &json!("aag/glm-5"))
            .expect("inserted");
        let patched = editor
            .finish(&json!({
                "note": "// not a comment",
                "url": "https://example.invalid/*x*/",
                "model": "aag/glm-5"
            }))
            .expect("verified");
        assert_eq!(
            patched,
            concat!(
                "{\n",
                "  \"note\": \"// not a comment\",\n",
                "  \"url\": \"https://example.invalid/*x*/\",\n",
                "  \"model\": \"aag/glm-5\"\n",
                "}\n",
            )
        );
    }

    #[test]
    fn malformed_documents_are_rejected() {
        for raw in [
            "{\"a\": 1, }",        // trailing comma
            "{ /* unterminated",   // unterminated block comment
            "{\"a\": [1,]}",       // trailing comma in array
            "[]",                  // non-object root
            "{\"a\": 1} trailing", // trailing garbage
            "",                    // empty document
        ] {
            assert!(JsoncEditor::new(raw).is_err(), "must reject {raw:?}");
        }
    }

    #[test]
    fn crlf_and_non_ascii_content_survive() {
        let raw = "{\r\n  // 备注 comment\r\n  \"名称\": \"值 💡\"\r\n}\r\n";
        let mut editor = JsoncEditor::new(raw).expect("parses");
        editor
            .set_top_level("model", &json!("aag/模型"))
            .expect("inserted");
        let patched = editor
            .finish(&json!({ "名称": "值 💡", "model": "aag/模型" }))
            .expect("verified");
        assert_eq!(
            patched,
            "{\r\n  // 备注 comment\r\n  \"名称\": \"值 💡\",\r\n  \"model\": \"aag/模型\"\r\n}\r\n"
        );
    }

    #[test]
    fn inline_objects_stay_on_one_line() {
        let raw = "{ \"provider\": {} }\n";
        let mut editor = JsoncEditor::new(raw).expect("parses");
        editor
            .set_provider_entry("aag", &json!({ "npm": "new" }))
            .expect("entry inserted");
        let patched = editor
            .finish(&json!({ "provider": { "aag": { "npm": "new" } } }))
            .expect("verified");
        assert_eq!(patched, "{ \"provider\": {\"aag\": {\"npm\":\"new\"}} }\n");
    }

    #[test]
    fn finish_rejects_a_mismatched_expected_value() {
        let mut editor = JsoncEditor::new("{\n  \"a\": 1\n}\n").expect("parses");
        editor.set_top_level("b", &json!(2)).expect("inserted");
        assert!(
            editor.finish(&json!({ "a": 1 })).is_err(),
            "a splice that disagrees with the intended value must be rejected"
        );
    }
}
