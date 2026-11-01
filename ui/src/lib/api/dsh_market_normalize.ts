import type { Market_catalog, Market_entry } from './dsh_market.js';
import classification_rules from '../../../../scripts/dsh_market/classification_rules.json';
import { registry_url, source_registry, max_entries } from './dsh_market_config.js';


const full_name_pattern = /^[A-Za-z0-9][A-Za-z0-9-]{0,38}\/[A-Za-z0-9_.-]{1,100}$/u;
const sha_pattern = /^[a-f0-9]{40}$/u;
const package_pattern = /^(?:@[a-z0-9][a-z0-9._-]*\/)?[a-z0-9][a-z0-9._-]*$/u;
const version_pattern = /^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/u;
const text_control_pattern = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/u;
const privacy_tokens = {
  authenticated_url: '[redacted_authenticated_url]',
  email: '[redacted_email]',
  user_path: '[redacted_user_path]',
  private_network: '[redacted_private_network]',
} as const;

type Feed_record = Record<string, unknown>;

type Classification_rule = { id: string; keywords: string[] };

function fail(code: string): never {
  throw new Error(`dsh_market:${code}`);
}

function is_record(value: unknown): value is Feed_record {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function read_text(value: unknown, max_length: number, optional = false): string | null {
  if ((value === null || value === undefined) && optional) return null;
  if (typeof value !== 'string' || value.length > max_length || text_control_pattern.test(value)) fail('invalid_text');
  return value;
}

function read_date(value: unknown, optional = false): string | null {
  const result = read_text(value, 64, optional);
  if (result !== null && (!/^\d{4}-\d{2}-\d{2}T/u.test(result) || Number.isNaN(Date.parse(result)))) fail('invalid_timestamp');
  return result;
}

function read_commit(value: unknown): string | null {
  const result = read_text(value, 40, true);
  if (result !== null && !sha_pattern.test(result)) fail('invalid_commit');
  return result;
}

function match_keyword(value: string, keyword: string): boolean {
  if (/^[a-z0-9]{1,3}$/u.test(keyword)) return value.split(/[^a-z0-9]+/u).includes(keyword);
  return value.includes(keyword);
}

function private_ipv4_address(value: string): boolean {
  const octets = value.split('.').map(Number);
  if (octets.length !== 4 || octets.some(octet => !Number.isInteger(octet) || octet < 0 || octet > 255)) return false;
  return octets[0] === 10
    || (octets[0] === 172 && octets[1] >= 16 && octets[1] <= 31)
    || (octets[0] === 192 && octets[1] === 168)
    || (octets[0] === 100 && octets[1] >= 64 && octets[1] <= 127);
}

function private_ipv6_address(value: string): boolean {
  let address = value.startsWith('[') ? value.slice(1, value.indexOf(']')) : value;
  address = address.split('%')[0];
  const embedded_ipv4 = address.match(/(\d{1,3}(?:\.\d{1,3}){3})$/u)?.[1];
  if (embedded_ipv4) {
    const octets = embedded_ipv4.split('.').map(Number);
    if (octets.some(octet => !Number.isInteger(octet) || octet < 0 || octet > 255)) return false;
    address = `${address.slice(0, -embedded_ipv4.length)}${((octets[0] << 8) | octets[1]).toString(16)}:${((octets[2] << 8) | octets[3]).toString(16)}`;
  }
  const halves = address.toLowerCase().split('::');
  if (halves.length > 2) return false;
  const left = halves[0] ? halves[0].split(':') : [];
  const right = halves.length === 2 && halves[1] ? halves[1].split(':') : [];
  const segments = halves.length === 2
    ? [...left, ...Array(8 - left.length - right.length).fill('0'), ...right]
    : left;
  if (segments.length !== 8 || segments.some(part => !/^[0-9a-f]{1,4}$/u.test(part))) return false;
  const values = segments.map(part => Number.parseInt(part, 16));
  const first = values[0];
  if ((first & 0xfe00) === 0xfc00 || (first & 0xffc0) === 0xfe80) return true;
  if (values.slice(0, 5).every(part => part === 0) && values[5] === 0xffff) {
    return private_ipv4_address([
      values[6] >> 8,
      values[6] & 0xff,
      values[7] >> 8,
      values[7] & 0xff,
    ].join('.'));
  }
  return false;
}

function classify(full_name: string, package_name: string | null, description: string, topics: string[]): Pick<Market_entry, 'categories' | 'use_cases' | 'modules' | 'classification_basis'> {
  const identity = `${full_name} ${package_name ?? ''}`.toLowerCase();
  const topic_text = topics.join(' ').toLowerCase();
  const normalized_description = description.toLowerCase();
  const rank = (rules: Classification_rule[], include_identity: boolean) => rules
    .map((rule, index) => ({
      id: rule.id,
      index,
      score: (rule.keywords.some(keyword => match_keyword(topic_text, keyword)) ? 5 : 0)
        + (include_identity && rule.keywords.some(keyword => match_keyword(identity, keyword)) ? 3 : 0)
        + (rule.keywords.some(keyword => match_keyword(normalized_description, keyword)) ? 1 : 0),
    }))
    .filter(rule => rule.score > 0)
    .sort((left, right) => right.score - left.score || left.index - right.index);
  const combined = `${identity} ${topic_text} ${normalized_description}`;
  const categories = rank(classification_rules.categories, true).slice(0, 3).map(rule => rule.id);
  const use_cases = classification_rules.use_cases
    .filter(rule => rule.keywords.some(keyword => match_keyword(combined, keyword))).map(rule => rule.id);
  const modules = classification_rules.modules
    .filter(rule => rule.keywords.some(keyword => match_keyword(combined, keyword))).map(rule => rule.id);
  return {
    categories: categories.length ? categories : ['other'],
    use_cases: use_cases.length ? use_cases : ['general'],
    modules: modules.length ? modules : ['general'],
    classification_basis: classification_rules.version as Market_entry['classification_basis'],
  };
}

function scrub_private_text(value: string): string {
  let result = value;
  result = result.replace(/\bhttps?:\/\/[^\s/@:]+:[^\s/@]+@[^\s"'<>]+/giu, privacy_tokens.authenticated_url);
  result = result.replace(/\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b/giu, privacy_tokens.email);
  result = result.replace(/\b[A-Za-z]:[\\/]Users[\\/][^\\/\s<>"']+[\\/][^\s<>"']+/gu, privacy_tokens.user_path);
  result = result.replace(/\/(?:Users|home)\/[^/\s<>"']+\/[^\s<>"']+/gu, privacy_tokens.user_path);
  result = result.replace(/\b(?:[a-z0-9-]+\.)+ts\.net\b/giu, privacy_tokens.private_network);
  result = result.replace(/\b\d{1,3}(?:\.\d{1,3}){3}\b/gu, match => private_ipv4_address(match) ? privacy_tokens.private_network : match);
  result = result.replace(/\[[0-9a-f:.]+\](?::\d{1,5})?/giu, match => private_ipv6_address(match) ? privacy_tokens.private_network : match);
  result = result.replace(/(?<![0-9a-f:])(?:[0-9a-f]{0,4}:){2,7}(?:[0-9a-f]{0,4}|\d{1,3}(?:\.\d{1,3}){3})(?![0-9a-f:])/giu, match => private_ipv6_address(match) ? privacy_tokens.private_network : match);
  return result;
}

function normalize_entry(raw: unknown, seen: Set<string>): Market_entry {
  if (!is_record(raw)) fail('invalid_entry');
  const full_name = read_text(raw.fullName, 140)!;
  if (!full_name_pattern.test(full_name) || full_name.split('/')[1] === '.' || full_name.split('/')[1] === '..') fail('invalid_repository');
  const id = full_name.toLowerCase();
  if (seen.has(id)) fail('duplicate_entry');
  seen.add(id);
  const repo_url = `https://github.com/${full_name}`;
  const supplied_url = read_text(raw.htmlUrl, 256, true);
  if (supplied_url !== null && supplied_url.toLowerCase() !== repo_url.toLowerCase()) fail('invalid_repository_url');
  const description = Array.from(read_text(raw.description ?? '', 16_000)!).slice(0, 400).join('');
  const description_en_raw = read_text(raw.descriptionEn ?? raw.description_en, 16_000, true);
  const description_zh_raw = read_text(raw.descriptionZh ?? raw.description_zh, 16_000, true);
  const package_name = read_text(raw.packageName, 214, true);
  const version = read_text(raw.version, 128, true);
  if (package_name !== null && !package_pattern.test(package_name)) fail('invalid_package');
  if (version !== null && !version_pattern.test(version)) fail('invalid_version');
  const source_commit = read_commit(raw.verifiedCommit ?? raw.sourceCommit ?? raw.source_commit);
  const topics = raw.topics ?? [];
  if (!Array.isArray(topics) || topics.length > 64) fail('invalid_topics');
  const normalized_topics = topics.map(topic => read_text(topic, 128)!);
  if (raw.stars !== undefined && (!Number.isSafeInteger(raw.stars) || (raw.stars as number) < 0)) fail('invalid_stars');
  const updated_at = read_date(raw.updatedAt, true);
  const safe_description = scrub_private_text(description);
  const safe_description_zh = description_zh_raw === null ? null : scrub_private_text(description_zh_raw).slice(0, 400);
  const has_chinese = /[\u3400-\u9fff]/u.test(description);
  const description_en = description_en_raw === null
    ? (!has_chinese ? safe_description || null : null)
    : scrub_private_text(description_en_raw).slice(0, 400);
  const description_zh = safe_description_zh ?? (has_chinese ? safe_description || null : null);
  const license_raw = read_text(raw.license, 256, true);
  const license = license_raw === null ? null : scrub_private_text(license_raw);
  if (raw.hasClient !== undefined && typeof raw.hasClient !== 'boolean') fail('invalid_client_flag');
  const install = raw.install === undefined || raw.install === null ? null : raw.install;
  if (install !== null && !is_record(install)) fail('invalid_install');
  const install_source = install === null ? null : read_text(install.source, 32, true);
  const install_mode = install === null ? null : read_text(install.mode, 32, true);
  const install_method: Market_entry['install_method'] = ['npm', 'github', 'manual'].includes(install_source ?? '')
    ? install_source as Market_entry['install_method']
    : install_mode === 'manual' ? 'manual' : 'unknown';
  const install_spec = install_method === 'npm' && package_name !== null && version !== null
    ? `${package_name}@${version}`
    : install_method === 'github' && source_commit !== null
      ? `github:${full_name}#${source_commit}`
      : null;
  return {
    id,
    name: full_name.split('/')[1],
    full_name,
    description: safe_description,
    description_en,
    description_zh,
    repo_url,
    source_commit,
    source_commit_url: source_commit ? `${repo_url}/tree/${source_commit}` : null,
    package_name,
    version,
    install_method,
    install_spec,
    ...classify(full_name, package_name, `${safe_description} ${description_en ?? ''} ${description_zh ?? ''}`, normalized_topics),
    author: full_name.split('/')[0],
    license,
    stars: (raw.stars as number | undefined) ?? 0,
    updated_at,
    has_client: (raw.hasClient as boolean | undefined) ?? false,
    verification: 'not_audited',
  };
}

export function normalize_market_feed(feed: unknown, fetched_at = new Date().toISOString()): Market_catalog {
  if (!is_record(feed) || feed.schemaVersion !== 2 || !Array.isArray(feed.plugins)) fail('invalid_feed');
  if (feed.plugins.length === 0 || feed.plugins.length > max_entries) fail('invalid_entry_count');
  read_date(fetched_at);
  const registry_generated_at = read_date(feed.generatedAt)!;
  const seen = new Set<string>();
  const entries = feed.plugins.map(record => normalize_entry(record, seen)).sort((left, right) => left.id.localeCompare(right.id));
  return {
    schema_version: 1,
    fetched_at,
    registry_generated_at,
    source_registry,
    registry_commit: '',
    registry_url,
    collector_url: '',
    entry_count: entries.length,
    entries,
    cache_status: 'refreshed',
    warning: null,
  };
}
