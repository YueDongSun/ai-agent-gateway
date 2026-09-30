import { spawnSync } from 'node:child_process';
import {
  chmodSync,
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  symlinkSync,
} from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join, parse, resolve } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { getConfigDir, getConfigPath, saveConfig, seedConfig } from '../src/config/store.js';

const originalConfigDir = process.env.AAG_CONFIG_DIR;
let testRoot = '';

function assertWindowsOwnerOnly(path: string): void {
  const systemRoot = process.env.SystemRoot;
  expect(systemRoot).toBeTruthy();
  const executable = join(
    systemRoot!,
    'System32',
    'WindowsPowerShell',
    'v1.0',
    'powershell.exe',
  );
  const script = String.raw`
$ErrorActionPreference = 'Stop'
$path = [System.IO.Path]::GetFullPath($env:AAG_TEST_ACL_PATH)
$item = Get-Item -LiteralPath $path -Force
if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) { exit 10 }
$acl = Get-Acl -LiteralPath $path
$current = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$owner = $acl.GetOwner([System.Security.Principal.SecurityIdentifier])
if ($owner.Value -ne $current.Value) { exit 11 }
if (-not $acl.AreAccessRulesProtected) { exit 12 }
$rules = @($acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
foreach ($rule in $rules) {
    if ($rule.IsInherited) { exit 13 }
    if (
        $rule.AccessControlType -eq [System.Security.AccessControl.AccessControlType]::Allow -and
        $rule.IdentityReference.Value -ne $current.Value
    ) { exit 14 }
}
exit 0
`;
  const environment = { ...process.env };
  for (const key of Object.keys(environment)) {
    if (key.toLowerCase() === 'psmodulepath') delete environment[key];
  }
  const result = spawnSync(
    executable,
    [
      '-NoLogo',
      '-NoProfile',
      '-NonInteractive',
      '-EncodedCommand',
      Buffer.from(script, 'utf16le').toString('base64'),
    ],
    {
      shell: false,
      windowsHide: true,
      env: { ...environment, AAG_TEST_ACL_PATH: resolve(path) },
    },
  );
  expect(result.status, result.stderr?.toString() || result.stdout?.toString()).toBe(0);
}

function windowsAclSddl(path: string): string {
  const executable = join(
    process.env.SystemRoot!,
    'System32',
    'WindowsPowerShell',
    'v1.0',
    'powershell.exe',
  );
  const script = String.raw`
$ErrorActionPreference = 'Stop'
$acl = Get-Acl -LiteralPath ([System.IO.Path]::GetFullPath($env:AAG_TEST_ACL_PATH))
[Console]::Out.Write($acl.GetSecurityDescriptorSddlForm(
  [System.Security.AccessControl.AccessControlSections]::Access
))
`;
  const environment = { ...process.env };
  for (const key of Object.keys(environment)) {
    if (key.toLowerCase() === 'psmodulepath') delete environment[key];
  }
  const result = spawnSync(
    executable,
    ['-NoLogo', '-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(script, 'utf16le').toString('base64')],
    {
      shell: false,
      windowsHide: true,
      encoding: 'utf8',
      env: { ...environment, AAG_TEST_ACL_PATH: resolve(path) },
    },
  );
  expect(result.status).toBe(0);
  return result.stdout.trim();
}

function assertPrivate(path: string, mode: number): void {
  if (process.platform === 'win32') {
    assertWindowsOwnerOnly(path);
  } else {
    expect(statSync(path).mode & 0o777).toBe(mode);
  }
}

beforeEach(() => {
  testRoot = mkdtempSync(join(tmpdir(), 'aag_node_private_storage_'));
  if (process.platform !== 'win32') chmodSync(testRoot, 0o777);
  process.env.AAG_CONFIG_DIR = join(testRoot, 'state');
});

afterEach(() => {
  if (originalConfigDir === undefined) {
    delete process.env.AAG_CONFIG_DIR;
  } else {
    process.env.AAG_CONFIG_DIR = originalConfigDir;
  }
  if (testRoot.startsWith(tmpdir()) && existsSync(testRoot)) {
    rmSync(testRoot, { recursive: true, force: true });
  }
});

describe.sequential('Node credential storage permissions', () => {
  it('rejects broad or relative overrides and treats an empty override as default', () => {
    process.env.AAG_CONFIG_DIR = '.';
    expect(() => getConfigDir()).toThrow('AAG_PRIVATE_STORAGE_CONFIG_DIRECTORY_NOT_ABSOLUTE');

    process.env.AAG_CONFIG_DIR = resolve(process.cwd());
    expect(() => getConfigDir()).toThrow('AAG_PRIVATE_STORAGE_CONFIG_DIRECTORY_TOO_BROAD');

    process.env.AAG_CONFIG_DIR = parse(resolve(process.cwd())).root;
    expect(() => getConfigDir()).toThrow('AAG_PRIVATE_STORAGE_CONFIG_DIRECTORY_TOO_BROAD');

    process.env.AAG_CONFIG_DIR = '   ';
    expect(getConfigDir()).toBe(join(homedir(), '.ai-agent-gateway'));
  });

  it('rejects an unsafe existing override directory without changing its permissions', () => {
    const override = join(testRoot, 'existing_general_directory');
    mkdirSync(override);
    process.env.AAG_CONFIG_DIR = override;
    if (process.platform === 'win32') {
      const before = windowsAclSddl(override);
      expect(() => saveConfig(seedConfig())).toThrow(/AAG_PRIVATE_STORAGE_WINDOWS_PERMISSIONS_UNSAFE/);
      expect(windowsAclSddl(override)).toBe(before);
    } else {
      chmodSync(override, 0o755);
      const before = statSync(override).mode & 0o777;
      expect(() => saveConfig(seedConfig())).toThrow(/AAG_PRIVATE_STORAGE_DIRECTORY_PERMISSIONS_UNSAFE/);
      expect(statSync(override).mode & 0o777).toBe(before);
    }
    expect(existsSync(join(override, 'config.json'))).toBe(false);
  });

  it('creates a private directory, atomic file, and one exact rolling backup', () => {
    const first = seedConfig();
    first.gateway.authToken = 'first-private-token';
    saveConfig(first);

    const second = seedConfig();
    second.gateway.authToken = 'second-private-token';
    saveConfig(second);

    const path = getConfigPath();
    const backup = `${path}.backup`;
    expect(readFileSync(path, 'utf8')).toContain('second-private-token');
    expect(readFileSync(backup, 'utf8')).toContain('first-private-token');
    expect(readFileSync(backup, 'utf8')).not.toContain('second-private-token');
    assertPrivate(process.env.AAG_CONFIG_DIR!, 0o700);
    assertPrivate(path, 0o600);
    assertPrivate(backup, 0o600);
    expect(
      readdirSync(process.env.AAG_CONFIG_DIR!).filter((name) => name.includes('.aag_tmp_')),
    ).toEqual([]);
    expect(existsSync(`${backup}.backup`)).toBe(false);
  });

  it.skipIf(process.platform === 'win32')(
    'preserves a tighter existing POSIX file mode across backup and replacement',
    () => {
      const first = seedConfig();
      first.gateway.authToken = 'first';
      saveConfig(first);
      chmodSync(getConfigPath(), 0o400);

      const second = seedConfig();
      second.gateway.authToken = 'second';
      saveConfig(second);

      expect(statSync(getConfigPath()).mode & 0o777).toBe(0o400);
      expect(statSync(`${getConfigPath()}.backup`).mode & 0o777).toBe(0o400);
    },
  );

  it('rejects a linked or reparse config directory before writing credentials', () => {
    const real = join(testRoot, 'real');
    const linked = join(testRoot, 'linked');
    const type: 'junction' | 'dir' = process.platform === 'win32' ? 'junction' : 'dir';
    mkdirSync(real);
    symlinkSync(real, linked, type);
    process.env.AAG_CONFIG_DIR = linked;

    expect(() => saveConfig(seedConfig())).toThrow('AAG_PRIVATE_STORAGE_LINKED_PATH');
    expect(existsSync(join(real, 'config.json'))).toBe(false);
  });
});
