/** JSON plus line/block comments and trailing commas; strings remain untouched.
 * No runtime JSONC dependency exists in the Node surface. This lexical pass
 * deliberately leaves all JSON grammar validation to JSON.parse.
 */
export function parse_jsonc_object(raw: string, label: string): Record<string, unknown> {
  const characters = raw.replace(/^\uFEFF/, '').split('');
  let in_string = false;
  for (let index = 0; index < characters.length; index += 1) {
    const character = characters[index];
    if (in_string) {
      if (character === '\\') index += 1;
      else if (character === '"') in_string = false;
      continue;
    }
    if (character === '"') { in_string = true; continue; }
    if (character !== '/') continue;
    if (characters[index + 1] === '/') {
      while (index < characters.length && !['\r', '\n'].includes(characters[index])) {
        characters[index] = ' ';
        index += 1;
      }
    } else if (characters[index + 1] === '*') {
      characters[index++] = ' ';
      characters[index++] = ' ';
      while (index < characters.length && !(characters[index] === '*' && characters[index + 1] === '/')) {
        if (!['\r', '\n'].includes(characters[index])) characters[index] = ' ';
        index += 1;
      }
      if (index >= characters.length) throw new Error(`${label} must contain valid JSONC (unterminated comment); original file was not changed`);
      characters[index] = characters[index + 1] = ' ';
      index += 1;
    }
  }
  in_string = false;
  for (let index = 0; index < characters.length; index += 1) {
    const character = characters[index];
    if (in_string) {
      if (character === '\\') index += 1;
      else if (character === '"') in_string = false;
    } else if (character === '"') in_string = true;
    else if (character === ',') {
      let next_index = index + 1;
      while (/\s/.test(characters[next_index] ?? '') && next_index < characters.length) next_index += 1;
      let previous_index = index - 1;
      while (previous_index >= 0 && /\s/.test(characters[previous_index])) previous_index -= 1;
      if ((characters[next_index] === '}' || characters[next_index] === ']') && !['{', '[', ','].includes(characters[previous_index])) characters[index] = ' ';
    }
  }
  let parsed: unknown;
  try { parsed = JSON.parse(characters.join('')); }
  catch { throw new Error(`${label} must contain valid JSONC; original file was not changed`); }
  if (!is_json_object(parsed)) throw new Error(`${label} must be a JSON object; original file was not changed`);
  return parsed;
}

export function is_json_object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
