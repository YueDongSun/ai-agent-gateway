/// JSONC parity test: pins the TypeScript stripper (`ui/src/lib/jsonc.ts`)
/// to the same fixture set the Rust unit test consumes
/// (`src-tauri/src/services/provider/config_editor.rs`, via include_str!),
/// so the two hand-maintained parsers of the same grammar cannot silently
/// drift. Add cases to the fixture — never inline here.
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { stripJsoncComments } from '../ui/src/lib/jsonc.js';

interface JsoncCase {
  name: string;
  raw: string;
  parses: boolean;
  contains: string[];
  not_contains: string[];
}

const fixturePath = fileURLToPath(new URL('./fixtures/jsonc_cases.json', import.meta.url));
const fixture = JSON.parse(readFileSync(fixturePath, 'utf8')) as {
  cases: JsoncCase[];
};

describe('jsonc stripper matches the shared Rust fixture', () => {
  for (const item of fixture.cases) {
    it(item.name, () => {
      const stripped = stripJsoncComments(item.raw);
      for (const needle of item.contains) {
        expect(stripped).toContain(needle);
      }
      for (const needle of item.not_contains) {
        expect(stripped).not.toContain(needle);
      }
      let parsed = false;
      try {
        JSON.parse(stripped);
        parsed = true;
      } catch {
        parsed = false;
      }
      expect(parsed).toBe(item.parses);
    });
  }
});
