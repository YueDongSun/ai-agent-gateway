/// JSONC text utilities shared by the Kilo direct-JSON editor (and any
/// future JSONC surface). The comment stripper mirrors the single Rust
/// implementation (`src-tauri/src/services/provider/config_editor.rs`) —
/// both sides are pinned to the same fixture set
/// (`tests/fixtures/jsonc_cases.json`) by the parity tests
/// (`tests/jsonc_parity.test.ts` and the Rust `shared_fixture_cases` test),
/// so the two grammars cannot silently drift.

/** Strip JSONC `//` line and `/* *​/` block comments outside string
 *  literals (escaped quotes respected) so `JSON.parse` can validate the
 *  text. Never used for writes — saves are verbatim. */
export function stripJsoncComments(raw: string): string {
  let out = '';
  let in_string = false;
  let escaped = false;
  for (let i = 0; i < raw.length; i += 1) {
    const c = raw[i];
    if (in_string) {
      out += c;
      if (escaped) escaped = false;
      else if (c === '\\') escaped = true;
      else if (c === '"') in_string = false;
      continue;
    }
    if (c === '"') {
      in_string = true;
      out += c;
    } else if (c === '/' && raw[i + 1] === '/') {
      while (i < raw.length && raw[i] !== '\n') i += 1;
      out += '\n';
    } else if (c === '/' && raw[i + 1] === '*') {
      i += 2;
      while (i < raw.length && !(raw[i] === '*' && raw[i + 1] === '/')) i += 1;
      i += 1;
    } else {
      out += c;
    }
  }
  return out;
}

/** Parse JSONC; returns an error message or null when valid. Enforces the
 *  object-root contract shared with the Rust save path. */
export function validateJsoncDocument(text: string): string | null {
  if (!text.trim()) return null;
  try {
    const parsed: unknown = JSON.parse(stripJsoncComments(text));
    if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
      return 'root_not_object';
    }
    return null;
  } catch (error) {
    return error instanceof SyntaxError ? error.message : 'invalid_json';
  }
}
