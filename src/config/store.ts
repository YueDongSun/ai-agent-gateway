import { spawnSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import {
  chmodSync,
  closeSync,
  existsSync,
  fstatSync,
  fsyncSync,
  lstatSync,
  mkdirSync,
  openSync,
  readFileSync,
  renameSync,
  unlinkSync,
  writeFileSync,
  type Stats,
} from 'node:fs';
import { basename, dirname, isAbsolute, join, parse, resolve } from 'node:path';
import { homedir } from 'node:os';
import { AppConfig, type AppConfig as AppConfigType } from './schema.js';

const PRIVATE_STORAGE_PREFIX = 'AAG_PRIVATE_STORAGE';
const PRIVATE_DIRECTORY_MODE = 0o700;
const PRIVATE_FILE_MODE = 0o600;
const TEMP_CREATE_ATTEMPTS = 4;

function configDir(): string {
  const override = process.env.AAG_CONFIG_DIR?.trim();
  const selected = override || join(homedir(), '.ai-agent-gateway');
  if (override && !isAbsolute(override)) {
    throw storageError('CONFIG_DIRECTORY_NOT_ABSOLUTE');
  }
  const absolute = resolve(selected);
  if (absolute === parse(absolute).root || absolute === resolve(process.cwd())) {
    throw storageError('CONFIG_DIRECTORY_TOO_BROAD');
  }
  return absolute;
}

function configDirIsManagedDefault(): boolean {
  return !process.env.AAG_CONFIG_DIR?.trim();
}

function configFile(): string {
  return join(configDir(), 'config.json');
}

function configBackupFile(): string {
  return join(configDir(), 'config.json.backup');
}

export function getConfigDir(): string {
  return configDir();
}

export function getConfigPath(): string {
  return configFile();
}

export function defaultConfig(): AppConfigType {
  return AppConfig.parse({
    gateway: { host: '127.0.0.1', port: 8787 },
    providers: [],
    routes: [],
    profiles: [],
    agents: [],
  });
}

/** Seed a brand-new config with a mock provider + route so the gateway works out-of-the-box. */
export function seedConfig(): AppConfigType {
  return AppConfig.parse({
    version: 1,
    gateway: { host: '127.0.0.1', port: 8787, authToken: '', logLevel: 'info' },
    providers: [
      {
        id: 'mock-primary',
        name: 'Mock Primary (slow)',
        type: 'mock',
        apiKey: 'mock',
        baseURL: 'mock://primary',
        models: ['mock-fast', 'mock-slow', 'claude-sonnet-4', 'gpt-4o'],
        enabled: true,
        weight: 1,
        circuitBreaker: {
          failureThreshold: 3,
          cooldownMs: 15_000,
          halfOpenRetries: 1,
          successThreshold: 2,
        },
      },
      {
        id: 'mock-fallback',
        name: 'Mock Fallback (always works)',
        type: 'mock',
        apiKey: 'mock',
        baseURL: 'mock://fallback',
        models: ['claude-sonnet-4', 'gpt-4o', 'mock-fast'],
        enabled: true,
        weight: 1,
        circuitBreaker: {
          failureThreshold: 5,
          cooldownMs: 30_000,
          halfOpenRetries: 1,
          successThreshold: 2,
        },
      },
    ],
    routes: [
      {
        alias: 'claude-sonnet-4',
        description: 'Sonnet via mock chain (primary -> fallback)',
        hops: [
          { provider: 'mock-primary', model: 'claude-sonnet-4' },
          { provider: 'mock-fallback', model: 'claude-sonnet-4' },
        ],
      },
      {
        alias: 'gpt-4o',
        description: 'GPT-4o via mock chain',
        hops: [
          { provider: 'mock-primary', model: 'gpt-4o' },
          { provider: 'mock-fallback', model: 'gpt-4o' },
        ],
      },
      {
        alias: 'mock-fail',
        description: 'Forces failures on primary to demo circuit-breaker',
        hops: [
          { provider: 'mock-primary', model: 'mock-fail' },
          { provider: 'mock-fallback', model: 'mock-fast' },
        ],
      },
    ],
    profiles: [],
    agents: [],
  });
}

function storageError(code: string, cause?: unknown): Error {
  const error = new Error(`${PRIVATE_STORAGE_PREFIX}_${code}`);
  error.name = 'PrivateStorageError';
  if (cause !== undefined) {
    (error as Error & { cause?: unknown }).cause = cause;
  }
  return error;
}

function lstatIfExists(path: string): Stats | undefined {
  try {
    return lstatSync(path);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
    throw storageError('METADATA_FAILED', error);
  }
}

function assertNoLinkedExistingComponents(path: string): void {
  let current = resolve(path);
  let leaf = true;
  for (;;) {
    const stats = lstatIfExists(current);
    if (stats?.isSymbolicLink()) throw storageError('LINKED_PATH');
    if (!leaf && stats && !stats.isDirectory()) {
      throw storageError('NON_DIRECTORY_COMPONENT');
    }
    const parent = dirname(current);
    if (parent === current) break;
    current = parent;
    leaf = false;
  }
}

function assertPosixOwner(stats: Stats): void {
  const getEffectiveUid = process.geteuid;
  if (typeof getEffectiveUid === 'function' && stats.uid !== getEffectiveUid()) {
    throw storageError('OWNER_MISMATCH');
  }
}

const WINDOWS_ACL_SCRIPT = String.raw`
$ErrorActionPreference = 'Stop'
try {
    $operation = $env:AAG_PRIVATE_ACL_OPERATION
    $literalPath = [System.IO.Path]::GetFullPath($env:AAG_PRIVATE_ACL_PATH)
    $sourcePath = $env:AAG_PRIVATE_ACL_SOURCE
    $kind = $env:AAG_PRIVATE_ACL_KIND
    $currentSid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User

    function Read-AagAcl([string]$path) {
        $item = Get-Item -LiteralPath $path -Force
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw 'linked_path'
        }
        return Get-Acl -LiteralPath $path
    }

    function Test-AagOwnerOnly($acl) {
        $owner = $acl.GetOwner([System.Security.Principal.SecurityIdentifier])
        if ($owner.Value -ne $currentSid.Value) { return $false }
        if (-not $acl.AreAccessRulesProtected) { return $false }
        $rules = @($acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
        foreach ($rule in $rules) {
            if ($rule.IsInherited) { return $false }
            if (
                $rule.AccessControlType -eq [System.Security.AccessControl.AccessControlType]::Allow -and
                $rule.IdentityReference.Value -ne $currentSid.Value
            ) {
                return $false
            }
        }
        return $true
    }

    if ($operation -eq 'copy') {
        $sourceAcl = Read-AagAcl ([System.IO.Path]::GetFullPath($sourcePath))
        if (-not (Test-AagOwnerOnly $sourceAcl)) { throw 'source_permissions_unsafe' }
        $targetAcl = Read-AagAcl $literalPath
        $targetOwner = $targetAcl.GetOwner([System.Security.Principal.SecurityIdentifier])
        if ($targetOwner.Value -ne $currentSid.Value) { throw 'owner_mismatch' }
        $sections = [System.Security.AccessControl.AccessControlSections]::Owner -bor
            [System.Security.AccessControl.AccessControlSections]::Group -bor
            [System.Security.AccessControl.AccessControlSections]::Access
        $sourceSddl = $sourceAcl.GetSecurityDescriptorSddlForm($sections)
        $targetAcl.SetSecurityDescriptorSddlForm($sourceSddl, $sections)
        [System.IO.File]::SetAccessControl($literalPath, $targetAcl)
        $actual = Read-AagAcl $literalPath
        if (-not (Test-AagOwnerOnly $actual)) { throw 'permissions_unsafe' }
        if (
            $sourceSddl -ne
            $actual.GetSecurityDescriptorSddlForm($sections)
        ) {
            throw 'permissions_mismatch'
        }
        exit 0
    }

    $acl = Read-AagAcl $literalPath
    $owner = $acl.GetOwner([System.Security.Principal.SecurityIdentifier])
    if ($owner.Value -ne $currentSid.Value) { throw 'owner_mismatch' }
    if ($operation -eq 'secure' -and -not (Test-AagOwnerOnly $acl)) {
        $acl.SetAccessRuleProtection($true, $false)
        $identities = @(
            $acl.GetAccessRules($true, $false, [System.Security.Principal.SecurityIdentifier]) |
                ForEach-Object { $_.IdentityReference } |
                Sort-Object -Property Value -Unique
        )
        foreach ($identity in $identities) {
            $acl.PurgeAccessRules($identity)
        }
        $inheritance = [System.Security.AccessControl.InheritanceFlags]::None
        if ($kind -eq 'directory') {
            $inheritance =
                [System.Security.AccessControl.InheritanceFlags]::ContainerInherit -bor
                [System.Security.AccessControl.InheritanceFlags]::ObjectInherit
        }
        $rule = New-Object System.Security.AccessControl.FileSystemAccessRule(
            $currentSid,
            [System.Security.AccessControl.FileSystemRights]::FullControl,
            $inheritance,
            [System.Security.AccessControl.PropagationFlags]::None,
            [System.Security.AccessControl.AccessControlType]::Allow
        )
        $acl.SetAccessRule($rule)
        $acl.SetOwner($currentSid)
        Set-Acl -LiteralPath $literalPath -AclObject $acl
        $acl = Read-AagAcl $literalPath
    }
    if (-not (Test-AagOwnerOnly $acl)) { throw 'permissions_unsafe' }
    exit 0
} catch {
    [Console]::Error.Write($_.Exception.Message)
    exit 1
}
`;

function windowsPowerShellPath(): string {
  const systemRoot = process.env.SystemRoot;
  if (!systemRoot) throw storageError('WINDOWS_ADAPTER_UNAVAILABLE');
  const executable = join(
    systemRoot,
    'System32',
    'WindowsPowerShell',
    'v1.0',
    'powershell.exe',
  );
  if (!existsSync(executable)) throw storageError('WINDOWS_ADAPTER_UNAVAILABLE');
  return executable;
}

function runWindowsAcl(
  operation: 'secure' | 'verify' | 'copy',
  path: string,
  kind: 'file' | 'directory',
  source?: string,
): void {
  const encodedScript = Buffer.from(WINDOWS_ACL_SCRIPT, 'utf16le').toString('base64');
  const environment = { ...process.env };
  for (const key of Object.keys(environment)) {
    // AAG can be launched from PowerShell 7. Its PSModulePath is incompatible
    // with the supported Windows PowerShell security module used here.
    if (key.toLowerCase() === 'psmodulepath') delete environment[key];
  }
  const result = spawnSync(
    windowsPowerShellPath(),
    ['-NoLogo', '-NoProfile', '-NonInteractive', '-EncodedCommand', encodedScript],
    {
      shell: false,
      windowsHide: true,
      encoding: 'utf8',
      timeout: 15_000,
      maxBuffer: 64 * 1024,
      env: {
        ...environment,
        AAG_PRIVATE_ACL_OPERATION: operation,
        AAG_PRIVATE_ACL_PATH: resolve(path),
        AAG_PRIVATE_ACL_KIND: kind,
        AAG_PRIVATE_ACL_SOURCE: source ? resolve(source) : '',
      },
    },
  );
  if (result.status === 0 && !result.error) return;
  const stderr = result.stderr ?? '';
  const known = [
    'linked_path',
    'owner_mismatch',
    'source_permissions_unsafe',
    'permissions_mismatch',
    'permissions_unsafe',
  ].find((code) => stderr.includes(code));
  if (known) throw storageError(`WINDOWS_${known.toUpperCase()}`);
  throw storageError('WINDOWS_ACL_FAILED', result.error);
}

function securePrivateDirectory(path: string, hardenExisting = false): void {
  assertNoLinkedExistingComponents(path);
  const existing = lstatIfExists(path);
  if (!existing) {
    try {
      mkdirSync(path, { recursive: true, mode: PRIVATE_DIRECTORY_MODE });
    } catch (error) {
      throw storageError('DIRECTORY_CREATE_FAILED', error);
    }
  }
  assertNoLinkedExistingComponents(path);
  const stats = lstatIfExists(path);
  if (!stats?.isDirectory()) throw storageError('DIRECTORY_EXPECTED');
  if (process.platform === 'win32') {
    runWindowsAcl(existing && !hardenExisting ? 'verify' : 'secure', path, 'directory');
    return;
  }
  assertPosixOwner(stats);
  const currentMode = stats.mode & 0o777;
  const desiredMode = existing ? currentMode & PRIVATE_DIRECTORY_MODE : PRIVATE_DIRECTORY_MODE;
  if (currentMode !== desiredMode) {
    if (existing && !hardenExisting) throw storageError('DIRECTORY_PERMISSIONS_UNSAFE');
    try {
      chmodSync(path, desiredMode);
    } catch (error) {
      throw storageError('DIRECTORY_PERMISSIONS_FAILED', error);
    }
  }
  const verified = lstatSync(path);
  assertPosixOwner(verified);
  if ((verified.mode & 0o077) !== 0) throw storageError('DIRECTORY_PERMISSIONS_UNSAFE');
}

function securePrivateFile(path: string, hardenExisting = false): Stats {
  assertNoLinkedExistingComponents(path);
  const stats = lstatIfExists(path);
  if (!stats?.isFile()) throw storageError('FILE_EXPECTED');
  if (process.platform === 'win32') {
    runWindowsAcl(hardenExisting ? 'secure' : 'verify', path, 'file');
    return lstatSync(path);
  }
  assertPosixOwner(stats);
  const desiredMode = (stats.mode & 0o777) & PRIVATE_FILE_MODE;
  if ((stats.mode & 0o777) !== desiredMode) {
    if (!hardenExisting) throw storageError('FILE_PERMISSIONS_UNSAFE');
    try {
      chmodSync(path, desiredMode);
    } catch (error) {
      throw storageError('FILE_PERMISSIONS_FAILED', error);
    }
  }
  const verified = lstatSync(path);
  assertPosixOwner(verified);
  if ((verified.mode & 0o177) !== 0) throw storageError('FILE_PERMISSIONS_UNSAFE');
  return verified;
}

function verifyPrivateFile(path: string): void {
  assertNoLinkedExistingComponents(path);
  const stats = lstatIfExists(path);
  if (!stats?.isFile()) throw storageError('FILE_EXPECTED');
  if (process.platform === 'win32') {
    runWindowsAcl('verify', path, 'file');
    return;
  }
  assertPosixOwner(stats);
  if ((stats.mode & 0o177) !== 0) throw storageError('FILE_PERMISSIONS_UNSAFE');
}

interface FileIdentity {
  dev: bigint;
  ino: bigint;
  birthtimeNs: bigint;
}

function identityFromPath(path: string): FileIdentity {
  const stats = lstatSync(path, { bigint: true });
  return { dev: stats.dev, ino: stats.ino, birthtimeNs: stats.birthtimeNs };
}

function identityFromDescriptor(descriptor: number): FileIdentity {
  const stats = fstatSync(descriptor, { bigint: true });
  return { dev: stats.dev, ino: stats.ino, birthtimeNs: stats.birthtimeNs };
}

function sameIdentity(left: FileIdentity, right: FileIdentity): boolean {
  return (
    left.dev === right.dev &&
    left.ino === right.ino &&
    left.birthtimeNs === right.birthtimeNs
  );
}

function applySourcePermissions(
  source: string,
  destination: string,
  hardenExisting: boolean,
): void {
  if (process.platform === 'win32') {
    runWindowsAcl('copy', destination, 'file', source);
    return;
  }
  const sourceStats = securePrivateFile(source, hardenExisting);
  const mode = (sourceStats.mode & 0o777) & PRIVATE_FILE_MODE;
  try {
    chmodSync(destination, mode);
  } catch (error) {
    throw storageError('FILE_PERMISSIONS_FAILED', error);
  }
}

function atomicPrivateWrite(
  path: string,
  data: string | Buffer,
  permissionSource?: string,
  hardenExisting = false,
): void {
  const parent = dirname(path);
  securePrivateDirectory(parent, hardenExisting);
  const existingStats = lstatIfExists(path);
  let targetIdentity: FileIdentity | undefined;
  if (existingStats) {
    securePrivateFile(path, hardenExisting);
    targetIdentity = identityFromPath(path);
  }

  let temporary: string | undefined;
  let descriptor: number | undefined;
  for (let attempt = 0; attempt < TEMP_CREATE_ATTEMPTS; attempt += 1) {
    const suffix = randomBytes(16).toString('hex');
    const candidate = join(parent, `.${basename(path)}.aag_tmp_${suffix}`);
    try {
      descriptor = openSync(candidate, 'wx', PRIVATE_FILE_MODE);
      temporary = candidate;
      break;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'EEXIST') continue;
      throw storageError('TEMPORARY_CREATE_FAILED', error);
    }
  }
  if (descriptor === undefined || temporary === undefined) {
    throw storageError('TEMPORARY_COLLISION');
  }

  let ownedIdentity: FileIdentity | undefined;
  let descriptorOpen = true;
  try {
    // Capture ownership before any ACL subprocess so an adapter failure can
    // still clean up only the exact empty file created by this operation.
    ownedIdentity = identityFromDescriptor(descriptor);
    if (process.platform === 'win32') {
      if (permissionSource) {
        applySourcePermissions(permissionSource, temporary, hardenExisting);
      } else {
        runWindowsAcl('secure', temporary, 'file');
      }
    } else {
      chmodSync(temporary, PRIVATE_FILE_MODE);
      if (permissionSource) applySourcePermissions(permissionSource, temporary, hardenExisting);
    }
    writeFileSync(descriptor, data);
    fsyncSync(descriptor);
    if (!sameIdentity(ownedIdentity, identityFromPath(temporary))) {
      throw storageError('TEMPORARY_CHANGED');
    }
    closeSync(descriptor);
    descriptorOpen = false;

    const currentTarget = lstatIfExists(path);
    if (
      (targetIdentity === undefined && currentTarget !== undefined) ||
      (targetIdentity !== undefined &&
        (currentTarget === undefined || !sameIdentity(targetIdentity, identityFromPath(path))))
    ) {
      throw storageError('TARGET_CHANGED');
    }
    try {
      renameSync(temporary, path);
    } catch (error) {
      throw storageError('ATOMIC_REPLACE_FAILED', error);
    }
    temporary = undefined;
    verifyPrivateFile(path);
    if (process.platform !== 'win32') {
      let parentDescriptor: number | undefined;
      try {
        parentDescriptor = openSync(parent, 'r');
        fsyncSync(parentDescriptor);
      } catch (error) {
        throw storageError('DIRECTORY_SYNC_FAILED', error);
      } finally {
        if (parentDescriptor !== undefined) closeSync(parentDescriptor);
      }
    }
  } finally {
    if (descriptorOpen) {
      try {
        closeSync(descriptor);
      } catch {
        // The stable operation error remains authoritative.
      }
    }
    if (temporary && ownedIdentity) {
      try {
        const current = lstatIfExists(temporary);
        if (current && sameIdentity(ownedIdentity, identityFromPath(temporary))) unlinkSync(temporary);
      } catch {
        // Never delete a path whose identity cannot be verified as ours.
      }
    }
  }
}

export function loadConfig(): AppConfigType {
  const path = configFile();
  const hardenExisting = configDirIsManagedDefault();
  if (!existsSync(path)) {
    const seeded = seedConfig();
    saveConfig(seeded);
    return seeded;
  }
  securePrivateDirectory(configDir(), hardenExisting);
  securePrivateFile(path, hardenExisting);
  const raw = readFileSync(path, 'utf8');
  try {
    return AppConfig.parse(JSON.parse(raw));
  } catch (err) {
    process.stderr.write(
      `[config] failed to parse the local configuration: ${(err as Error).message}\n` +
        'falling back to seed config.\n',
    );
    return seedConfig();
  }
}

export function saveConfig(cfg: AppConfigType): void {
  const directory = configDir();
  const path = configFile();
  const hardenExisting = configDirIsManagedDefault();
  securePrivateDirectory(directory, hardenExisting);
  if (existsSync(path)) {
    securePrivateFile(path, hardenExisting);
    atomicPrivateWrite(configBackupFile(), readFileSync(path), path, hardenExisting);
    atomicPrivateWrite(path, JSON.stringify(cfg, null, 2) + '\n', path, hardenExisting);
    return;
  }
  atomicPrivateWrite(path, JSON.stringify(cfg, null, 2) + '\n', undefined, hardenExisting);
}
