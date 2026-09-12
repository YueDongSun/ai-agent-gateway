/// JSON syntax highlighting + live validation.
///
/// `highlightJson` produces an HTML string with spans (token classes:
/// json-key / json-string / json-number / json-boolean / json-null) for
/// use inside <pre dangerouslySetInnerHTML>. Input is escaped first, so
/// the output is safe as long as the classes below are the only markup.
///
/// `validateJson` parses and returns a { ok, error?, value? } result used
/// by the raw editor to gate the Save button.

export interface JsonValidation {
  ok: boolean;
  error?: string;
  value?: unknown;
}

export function validateJson(text: string): JsonValidation {
  if (!text.trim()) return { ok: false, error: 'empty' };
  try {
    const value = JSON.parse(text);
    return { ok: true, value };
  } catch (e) {
    return { ok: false, error: String(e instanceof Error ? e.message : e) };
  }
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}

/// Tokenize JSON text and wrap tokens in class spans. Operates on the
/// escaped string so no raw HTML can be injected through key names.
export function highlightJson(text: string): string {
  const escaped = escapeHtml(text);
  // Match strings (keys when followed by ':'), numbers, booleans, null.
  return escaped.replace(
    /("(?:[^"\\]|\\.)*")(\s*:)?|\b(true|false)\b|\bnull\b|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/g,
    (match, str, colon, bool, num) => {
      if (str !== undefined && str !== null) {
        if (colon) return `<span class="json-key">${str}</span>${colon}`;
        return `<span class="json-string">${str}</span>`;
      }
      if (bool) return `<span class="json-boolean">${bool}</span>`;
      if (num) return `<span class="json-number">${num}</span>`;
      return match;
    },
  );
}