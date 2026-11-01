/**
 * Deterministic local token estimate for the Anthropic count_tokens
 * surface (Rust proxy parity: `estimate_input_tokens` in
 * src-tauri/src/proxy/providers/mod.rs). ~4 chars per token for ASCII-ish
 * text, ~1 token per CJK character; structural keys count toward the
 * total. Purely local — no network, no secrets read.
 */
export function estimate_input_tokens(body: unknown): number {
  let ascii_chars = 0;
  let wide_chars = 0;
  const is_wide = (code: number): boolean =>
    (code >= 0x1100 && code <= 0x115f) ||
    (code >= 0x2e80 && code <= 0xa4cf) ||
    (code >= 0xac00 && code <= 0xd7a3) ||
    (code >= 0xf900 && code <= 0xfaff) ||
    (code >= 0xfe30 && code <= 0xfe4f) ||
    (code >= 0xff00 && code <= 0xff60) ||
    (code >= 0xffe0 && code <= 0xffe6) ||
    (code >= 0x20000 && code <= 0x3fffd);
  const walk = (value: unknown): void => {
    if (typeof value === 'string') {
      for (const character of value) {
        if (is_wide(character.codePointAt(0) ?? 0)) wide_chars += 1;
        else ascii_chars += 1;
      }
    } else if (Array.isArray(value)) {
      value.forEach(walk);
    } else if (value && typeof value === 'object') {
      for (const [key, item] of Object.entries(value as Record<string, unknown>)) {
        // Structural keys count toward the total too.
        ascii_chars += key.length;
        walk(item);
      }
    }
  };
  walk(body);
  return Math.max(Math.floor(ascii_chars / 4) + wide_chars, 1);
}
