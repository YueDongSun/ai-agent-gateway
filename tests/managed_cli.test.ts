import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

describe('launcher native command availability', () => {
  for (const { source_module, expected_commands } of [
    { source_module: 'managed_cli', expected_commands: ['managed_cli_status', 'managed_cli_open_terminal'] },
    { source_module: 'gemini', expected_commands: ['gemini_open_cli_terminal'] },
  ]) it(`registers and permits ${source_module} commands only in the trusted manager`, () => {
    const frontend = readFileSync(resolve(`ui/src/lib/api/${source_module}.ts`), 'utf8');
    const handler = readFileSync(resolve('src-tauri/src/lib.rs'), 'utf8');
    const manifest = readFileSync(resolve('src-tauri/build.rs'), 'utf8');
    const manager = JSON.parse(readFileSync(resolve('src-tauri/capabilities/default.json'), 'utf8'));
    const marketplace = JSON.parse(readFileSync(resolve('src-tauri/capabilities/dsh_market.json'), 'utf8'));
    const commands = [...frontend.matchAll(/invoke<[^>]+>\('([^']+)'/g)]
      .map((match) => match[1]);
    expect(commands).toEqual(expected_commands);
    expect(manager.windows).toEqual(['main']);
    for (const command of commands) {
      const permission = `allow-${command.replaceAll('_', '-')}`;
      expect(handler).toContain(`commands::launcher::${command}`);
      expect(manifest).toContain(`"${command}"`);
      expect(manager.permissions).toContain(permission);
      expect(marketplace.permissions).not.toContain(permission);
    }
  });
});
