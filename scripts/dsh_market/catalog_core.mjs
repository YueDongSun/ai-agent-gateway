/** Read-only registry adapter. Feed content is data, never executable instructions. */
import { readFile } from 'node:fs/promises';
import { isIP } from 'node:net';

export const source_registry = 'YELEBAI/dsh-plugin-marketplace';
export const max_feed_bytes = 24 * 1024 * 1024;
export const max_catalog_bytes = 12 * 1024 * 1024;
export const max_entries = 20_000;
export const refresh_timeout_ms = 30_000;
export const rules = JSON.parse(await readFile(new URL('./classification_rules.json', import.meta.url), 'utf8'));
const full_name_pattern = /^[A-Za-z0-9][A-Za-z0-9-]{0,38}\/[A-Za-z0-9_.-]{1,100}$/;
const sha_pattern = /^[a-f0-9]{40}$/;
const package_pattern = /^(?:@[a-z0-9][a-z0-9._-]*\/)?[a-z0-9][a-z0-9._-]*$/;
const version_pattern = /^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;
export const privacy_replacement_tokens = Object.freeze({
  authenticated_url: '[redacted_authenticated_url]',
  email: '[redacted_email]',
  user_path: '[redacted_user_path]',
  private_network: '[redacted_private_network]',
});
const privacy_fields = Object.freeze(['description', 'description_en', 'description_zh', 'license']);

function fail(code) { throw new Error(`dsh_market:${code}`); }
function text(value, max_length, optional = false) {
  if ((value === null || value === undefined) && optional) return null;
  if (typeof value !== 'string' || value.length > max_length || /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/.test(value)) fail('invalid_text');
  return value;
}
function date(value, optional = false) {
  const result = text(value, 64, optional);
  if (result !== null && (!/^\d{4}-\d{2}-\d{2}T/.test(result) || Number.isNaN(Date.parse(result)))) fail('invalid_timestamp');
  return result;
}
function sha(value, optional = false) {
  const result = text(value, 40, optional);
  if (result !== null && !sha_pattern.test(result)) fail('invalid_commit');
  return result;
}
function match_keyword(value, keyword) {
  if (/^[a-z0-9]{1,3}$/.test(keyword)) return value.split(/[^a-z0-9]+/).includes(keyword);
  return value.includes(keyword);
}

function empty_privacy_counts() {
  return { authenticated_url: 0, email: 0, user_path: 0, private_network: 0 };
}

function private_network_address(value) {
  const octets = value.split('.').map(Number);
  if (octets.length !== 4 || octets.some(octet => !Number.isInteger(octet) || octet < 0 || octet > 255)) return false;
  return octets[0] === 10
    || (octets[0] === 172 && octets[1] >= 16 && octets[1] <= 31)
    || (octets[0] === 192 && octets[1] === 168)
    || (octets[0] === 100 && octets[1] >= 64 && octets[1] <= 127);
}

function private_network_ipv6(value) {
  const address = value.startsWith('[')
    ? value.slice(1, value.indexOf(']'))
    : value;
  if (isIP(address) !== 6) return false;
  let normalized = address.toLowerCase();
  const embedded_ipv4 = normalized.match(/(\d{1,3}(?:\.\d{1,3}){3})$/u)?.[1];
  if (embedded_ipv4) {
    const octets = embedded_ipv4.split('.').map(Number);
    normalized = `${normalized.slice(0, -embedded_ipv4.length)}${((octets[0] << 8) | octets[1]).toString(16)}:${((octets[2] << 8) | octets[3]).toString(16)}`;
  }
  const halves = normalized.split('::');
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
    return private_network_address([
      values[6] >> 8,
      values[6] & 0xff,
      values[7] >> 8,
      values[7] & 0xff,
    ].join('.'));
  }
  return false;
}

/** Replace privacy-bearing description fragments without exposing the matches. */
export function privacy_scrub_text(value) {
  if (typeof value !== 'string') fail('invalid_text');
  const counts = empty_privacy_counts();
  const replace = (input, pattern, category, predicate = () => true) => input.replace(pattern, match => {
    if (!predicate(match)) return match;
    counts[category] += 1;
    return privacy_replacement_tokens[category];
  });
  let scrubbed = value;
  scrubbed = replace(scrubbed, /\bhttps?:\/\/[^\s/@:]+:[^\s/@]+@[^\s"'<>]+/giu, 'authenticated_url');
  scrubbed = replace(scrubbed, /\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b/giu, 'email');
  scrubbed = replace(scrubbed, /\b[A-Za-z]:[\\/]Users[\\/][^\\/\s<>"']+[\\/][^\s<>"']+/gu, 'user_path');
  scrubbed = replace(scrubbed, /\/(?:Users|home)\/[^/\s<>"']+\/[^\s<>"']+/gu, 'user_path');
  scrubbed = replace(scrubbed, /\b(?:[a-z0-9-]+\.)+ts\.net\b/giu, 'private_network');
  scrubbed = replace(scrubbed, /\[[0-9a-f:]+\](?::\d{1,5})?/giu, 'private_network', private_network_ipv6);
  scrubbed = replace(scrubbed, /(?<![0-9a-f:])(?:[0-9a-f]{0,4}:){2,7}(?:[0-9a-f]{0,4}|\d{1,3}(?:\.\d{1,3}){3})(?![0-9a-f:])/giu, 'private_network', private_network_ipv6);
  scrubbed = replace(scrubbed, /\b\d{1,3}(?:\.\d{1,3}){3}\b/gu, 'private_network', private_network_address);
  return { value: scrubbed, counts };
}

function privacy_summary(source_entry_count, modified_entries, counts) {
  return {
    schema_version: 1,
    fields: [...privacy_fields],
    replacement_tokens: privacy_replacement_tokens,
    source_entry_count,
    modified_entries,
    replacements: counts,
    total_replacements: Object.values(counts).reduce((sum, count) => sum + count, 0),
  };
}

function create_privacy_accumulator(source_entry_count, prior, modified_floor = 0) {
  const counts = empty_privacy_counts();
  if (prior?.schema_version === 1 && prior.replacements) {
    for (const category of Object.keys(counts)) {
      const count = prior.replacements[category];
      if (Number.isSafeInteger(count) && count >= 0) counts[category] = count;
    }
  }
  const modified = new Set();
  return {
    scrub(value, entry_id) {
      if (value === null) return null;
      const result = privacy_scrub_text(value);
      for (const category of Object.keys(counts)) counts[category] += result.counts[category];
      if (result.value !== value) modified.add(entry_id);
      return result.value;
    },
    summary() { return privacy_summary(source_entry_count, Math.max(modified_floor, modified.size), counts); },
  };
}
export function classify(entry) {
  const identity = `${entry.full_name} ${entry.package_name ?? ''}`.toLowerCase();
  const topics = entry.topics.join(' ').toLowerCase();
  const description = entry.description.toLowerCase();
  const ranked = rules.categories.map((rule, index) => ({
    id: rule.id, index,
    score: (rule.keywords.some(keyword => match_keyword(topics, keyword)) ? 5 : 0)
      + (rule.keywords.some(keyword => match_keyword(identity, keyword)) ? 3 : 0)
      + (rule.keywords.some(keyword => match_keyword(description, keyword)) ? 1 : 0),
  })).filter(rule => rule.score > 0).sort((a, b) => b.score - a.score || a.index - b.index);
  const combined = `${identity} ${topics} ${description}`;
  const use_cases = rules.use_cases.filter(rule => rule.keywords.some(keyword => match_keyword(combined, keyword))).map(rule => rule.id);
  const modules = rules.modules.filter(rule => rule.keywords.some(keyword => match_keyword(combined, keyword))).map(rule => rule.id);
  return { categories: ranked.length ? ranked.slice(0, 3).map(rule => rule.id) : ['other'], use_cases: use_cases.length ? use_cases : ['general'], modules: modules.length ? modules : ['general'] };
}

/** Normalize only the allowlisted feed schema; reject the whole update on invalid data. */
export function normalize_feed(bytes, registry_commit, fetched_at = new Date().toISOString()) {
  if (Buffer.byteLength(bytes) > max_feed_bytes) fail('feed_too_large');
  let feed;
  try { feed = JSON.parse(bytes); } catch { fail('invalid_json'); }
  if (!feed || ![1, 2].includes(feed.schemaVersion) || !Array.isArray(feed.plugins)) fail('invalid_feed');
  if (feed.plugins.length === 0 || feed.plugins.length > max_entries) fail('invalid_entry_count');
  sha(registry_commit); date(fetched_at);
  const registry_generated_at = date(feed.generatedAt);
  const seen = new Set();
  const privacy = create_privacy_accumulator(feed.plugins.length);
  const entries = feed.plugins.map(raw => {
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) fail('invalid_entry');
    const full_name = text(raw.fullName, 140);
    if (!full_name_pattern.test(full_name) || full_name.split('/')[1] === '.' || full_name.split('/')[1] === '..') fail('invalid_repository');
    const id = full_name.toLowerCase();
    if (seen.has(id)) fail('duplicate_entry');
    seen.add(id);
    const repo_url = `https://github.com/${full_name}`;
    if (text(raw.htmlUrl, 256).toLowerCase() !== repo_url.toLowerCase()) fail('invalid_repository_url');
    const description_raw = Array.from(text(raw.description ?? '', 16_000)).slice(0, 400).join('');
    const package_name = text(raw.packageName, 214, true);
    const version = text(raw.version, 128, true);
    if (package_name !== null && !package_pattern.test(package_name)) fail('invalid_package');
    if (version !== null && privacy_scrub_text(version).value !== version) fail('invalid_version_privacy');
    const source_commit = sha(raw.verifiedCommit, true);
    const topics = raw.topics ?? [];
    if (!Array.isArray(topics) || topics.length > 100) fail('invalid_topics');
    topics.forEach(topic => text(topic, 128));
    if (raw.stars !== undefined && (!Number.isSafeInteger(raw.stars) || raw.stars < 0)) fail('invalid_stars');
    if (raw.hasClient !== undefined && typeof raw.hasClient !== 'boolean') fail('invalid_client_flag');
    const install_source = raw.install?.source;
    const install_method = ['npm', 'github', 'manual'].includes(install_source) ? install_source : raw.install?.mode === 'manual' ? 'manual' : 'unknown';
    // Construct the display spec; never pass arbitrary registry shell instructions through.
    const install_spec = install_method === 'npm' && package_pattern.test(package_name ?? '') && version_pattern.test(version ?? '')
      ? `${package_name}@${version}`
      : install_method === 'github' && source_commit ? `github:${full_name}#${source_commit}` : null;
    const description_en_raw = text(raw.descriptionEn ?? raw.description_en, 16_000, true);
    const description_en_unredacted = description_en_raw === null ? null : Array.from(description_en_raw).slice(0, 400).join('');
    const description_zh_raw = text(raw.descriptionZh ?? raw.description_zh, 16_000, true);
    const description_zh_unredacted = description_zh_raw === null ? null : Array.from(description_zh_raw).slice(0, 400).join('');
    const description = privacy.scrub(description_raw, id);
    const description_en = privacy.scrub(description_en_unredacted, id);
    const description_zh = privacy.scrub(description_zh_unredacted, id);
    const license = privacy.scrub(text(raw.license, 256, true), id);
    const classified = classify({ full_name, package_name, description, topics });
    return {
      id, name: full_name.split('/')[1], full_name, description,
      description_en: description_en ?? (!/[\u3400-\u9fff]/.test(description) ? description || null : null),
      description_zh: description_zh ?? (/[\u3400-\u9fff]/.test(description) ? description || null : null),
      repo_url, source_commit, source_commit_url: source_commit ? `${repo_url}/tree/${source_commit}` : null,
      package_name, version, install_method, install_spec,
      ...classified, classification_basis: rules.version, author: full_name.split('/')[0],
      license, stars: raw.stars ?? 0,
      updated_at: date(raw.updatedAt, true), has_client: raw.hasClient ?? false, verification: 'not_audited',
    };
  }).sort((a, b) => a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
  return {
    schema_version: 1, fetched_at, registry_generated_at, source_registry, registry_commit,
    registry_url: `https://github.com/${source_registry}/blob/${registry_commit}/registry/plugins.json`,
    collector_url: `https://github.com/${source_registry}/blob/${registry_commit}/scripts/registry-scan.mjs`,
    entry_count: entries.length, entries, cache_status: 'bundled', warning: null,
    privacy_scrub: privacy.summary(),
  };
}

/** Re-scrub a previously normalized snapshot without a network request or entry loss. */
export function sanitize_catalog_snapshot(catalog) {
  if (!catalog || catalog.schema_version !== 1 || !Array.isArray(catalog.entries)
      || catalog.entries.length === 0 || catalog.entry_count !== catalog.entries.length
      || !sha_pattern.test(catalog.registry_commit ?? '')) fail('invalid_catalog');
  const privacy = create_privacy_accumulator(
    catalog.entries.length,
    catalog.privacy_scrub,
    catalog.privacy_scrub?.modified_entries ?? 0,
  );
  const entries = catalog.entries.map(entry => {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) fail('invalid_catalog');
    const id = text(entry.id, 140);
    const description = privacy.scrub(text(entry.description, 400), id);
    const description_en = privacy.scrub(text(entry.description_en, 400, true), id);
    const description_zh = privacy.scrub(text(entry.description_zh, 400, true), id);
    const package_name = text(entry.package_name, 214, true);
    const version = text(entry.version, 128, true);
    if (package_name !== null && !package_pattern.test(package_name)) fail('invalid_package');
    if (version !== null && privacy_scrub_text(version).value !== version) fail('invalid_version_privacy');
    const license = privacy.scrub(text(entry.license, 256, true), id);
    return { ...entry, description, description_en, description_zh, license };
  });
  const summary = privacy.summary();
  if (catalog.privacy_scrub?.schema_version === 1
      && JSON.stringify(catalog.privacy_scrub) === JSON.stringify(summary)
      && entries.every((entry, index) => JSON.stringify(entry) === JSON.stringify(catalog.entries[index]))) {
    return catalog;
  }
  return { ...catalog, entries, privacy_scrub: summary };
}

export function fetch_url_allowed(value) {
  try {
    const target = new URL(value);
    if (target.protocol !== 'https:' || target.username || target.password || target.port || target.search || target.hash) return false;
    return value === `https://api.github.com/repos/${source_registry}/commits/main`
      || (target.hostname === 'raw.githubusercontent.com' && new RegExp(`^/${source_registry}/[a-f0-9]{40}/registry/plugins\\.json$`).test(target.pathname));
  } catch { return false; }
}

export async function fetch_bounded(url, { signal, fetch_impl = fetch, limit = max_feed_bytes } = {}) {
  if (!fetch_url_allowed(url)) fail('fetch_url_rejected');
  const response = await fetch_impl(url, { signal, redirect: 'error', credentials: 'omit', headers: { accept: 'application/json', 'user-agent': 'aag-dsh-market-catalog' } });
  if (!response.ok) fail(response.status === 403 || response.status === 429 ? 'registry_rate_limited' : 'registry_http_error');
  if (Number(response.headers.get('content-length') ?? 0) > limit) { await response.body?.cancel(); fail('feed_too_large'); }
  if (!response.body) fail('empty_response');
  const reader = response.body.getReader();
  let length = 0; const chunks = [];
  try {
    while (true) {
      const { done, value } = await reader.read(); if (done) break;
      length += value.length;
      if (length > limit) { await reader.cancel(); fail('feed_too_large'); }
      chunks.push(value);
    }
  } finally { reader.releaseLock(); }
  try { return new TextDecoder('utf-8', { fatal: true }).decode(Buffer.concat(chunks, length)); }
  catch { fail('invalid_utf8'); }
}

export async function collect_catalog({ fetch_impl = fetch, fetched_at } = {}) {
  const signal = AbortSignal.timeout(refresh_timeout_ms);
  let commit_response;
  try { commit_response = JSON.parse(await fetch_bounded(`https://api.github.com/repos/${source_registry}/commits/main`, { signal, fetch_impl, limit: 2 * 1024 * 1024 })); }
  catch (error) { if (error instanceof SyntaxError) fail('invalid_commit_response'); throw error; }
  const registry_commit = sha(commit_response.sha);
  const bytes = await fetch_bounded(`https://raw.githubusercontent.com/${source_registry}/${registry_commit}/registry/plugins.json`, { signal, fetch_impl });
  return normalize_feed(bytes, registry_commit, fetched_at);
}
