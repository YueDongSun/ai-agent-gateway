import { readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import ts from 'typescript';
import { describe, expect, it } from 'vitest';

const cli_directory = resolve('ui/src/components/app');
const navigation_modules = ['ui/src/App.tsx', 'ui/src/components/workspace_header.tsx', 'ui/src/lib/workspace_routes.ts'];

function imports_of(path: string): string[] {
  const source = ts.createSourceFile(path, readFileSync(path, 'utf8'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  return source.statements.filter(ts.isImportDeclaration)
    .map((statement) => (statement.moduleSpecifier as ts.StringLiteral).text);
}

describe('CLI presentation ownership', () => {
  it('keeps provider presentation modules out of the CLI subtree', () => {
    const files = readdirSync(cli_directory);
    expect(files.some((name) => /provider|route_editor|gateway|launcher|preset|config_editor/i.test(name))).toBe(false);
    for (const file of files.filter((name) => /\.tsx?$/.test(name))) {
      const imports = imports_of(resolve(cli_directory, file));
      expect(imports.filter((path) => /api\/providers|suppliers\/client_management|appPages|unified_providers|providers_state|routes_state/.test(path))).toEqual([]);
    }
  });

  it('navigation imports client labels rather than provider form or card mappings', () => {
    for (const path of navigation_modules) {
      expect(imports_of(path).filter((name) => /appPages|unified_providers|api\/providers|providers_state/.test(name))).toEqual([]);
    }
  });

  it('the supplier boundary is the only importer of the management workspace', () => {
    const source = readFileSync('ui/src/components/suppliers/client_management_section.tsx', 'utf8');
    expect(source).toContain("import('./client_management/supplier_client_settings.js')");
    expect(readFileSync('ui/src/App.tsx', 'utf8')).not.toContain('supplier_client_settings');
  });
});
