import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile, mkdtemp, writeFile, readdir, rm, mkdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { normalize_feed, fetch_bounded, collect_catalog, fetch_url_allowed, max_feed_bytes, max_catalog_bytes, max_entries, privacy_replacement_tokens, privacy_scrub_text, sanitize_catalog_snapshot, source_registry } from './catalog_core.mjs';
import { write_catalog } from './collect_catalog.mjs';

const fixture = JSON.parse(await readFile(new URL('./test_feed.json', import.meta.url), 'utf8'));
const registry_commit = 'a'.repeat(40);
const fetched_at = '2026-08-30T09:00:00.000Z';
const normalize = feed => normalize_feed(JSON.stringify(feed), registry_commit, fetched_at);

test('provenance distinguishes the registry commit from each plugin commit and unknown commits', () => {
  const catalog = normalize(fixture);
  assert.equal(catalog.entry_count, 2);
  assert.equal(catalog.registry_commit, registry_commit);
  assert.equal(catalog.entries[0].source_commit, 'b'.repeat(40));
  assert.equal(catalog.entries[1].source_commit, null);
  assert.equal(catalog.entries[1].source_commit_url, null);
  assert.ok(catalog.registry_url.includes(`/blob/${registry_commit}/`));
  assert.ok(catalog.entries.every(entry => entry.verification === 'not_audited'));
});
test('classifies function and practical uses, retains supplied translations without invented translations', () => {
  const entries = normalize(fixture).entries;
  assert.ok(entries[0].categories.includes('models'));
  assert.ok(entries[0].use_cases.includes('claude_native'));
  assert.ok(entries[0].use_cases.includes('desktop_workflow'));
  assert.equal(entries[0].description_zh, fixture.plugins[0].descriptionZh);
  assert.equal(entries[1].description_en, null);
  assert.ok(entries[1].categories.includes('personalization'));
});
test('assigns practical modules with the general fallback and the v2 basis', () => {
  const entries = normalize(fixture).entries;
  // example/claude-desktop-tools: claude+mcp+desktop → models & tools modules.
  assert.ok(entries[0].modules.includes('models'));
  assert.ok(entries[0].modules.includes('tools'));
  // example/theme-memory: 主题/界面/美化 + memory → appearance & knowledge.
  assert.ok(entries[1].modules.includes('appearance'));
  assert.ok(entries[1].modules.includes('knowledge'));
  assert.ok(entries.every(entry => entry.classification_basis === 'metadata_rules_v2'));
  // A plugin matching no module keyword still lands in the general module.
  const feed = structuredClone(fixture);
  feed.plugins[1] = { ...feed.plugins[1], fullName: 'example/zz-unmatched', htmlUrl: 'https://github.com/example/zz-unmatched', description: 'zzz qqq xwx', packageName: 'zz-unmatched', topics: ['zzzxwx'] };
  assert.deepEqual(normalize(feed).entries.find(entry => entry.id === 'example/zz-unmatched').modules, ['general']);
});
test('constructs exact display specs and never trusts arbitrary registry install instructions', () => {
  assert.equal(normalize(fixture).entries[0].install_spec, '@example/claude-desktop-tools@1.2.3');
  const feed = structuredClone(fixture); feed.plugins[0].version = 'latest; echo unsafe';
  assert.equal(normalize(feed).entries[0].install_spec, null);
});
test('rejects hostile repository schemes, credential URLs, paths and duplicate ids', () => {
  for (const bad of ['javascript:alert(1)', 'https://github.com@evil.example/x', 'https://github.com/example/claude-desktop-tools?token=x']) {
    const feed = structuredClone(fixture); feed.plugins[0].htmlUrl = bad;
    assert.throws(() => normalize(feed), /invalid_repository_url/);
  }
  const duplicate = structuredClone(fixture); duplicate.plugins.push({...duplicate.plugins[0], fullName:'Example/CLAUDE-desktop-tools'});
  assert.throws(() => normalize(duplicate), /duplicate_entry/);
  const path = structuredClone(fixture); path.plugins[0].fullName = 'example/../x';
  assert.throws(() => normalize(path), /invalid_repository/);
});
test('rejects empty, oversized and malformed feeds without silently dropping entries', () => {
  assert.throws(() => normalize({...fixture, plugins:[]}), /invalid_entry_count/);
  assert.throws(() => normalize({...fixture, plugins:Array(max_entries + 1).fill({})}), /invalid_entry_count/);
  assert.throws(() => normalize_feed(' '.repeat(max_feed_bytes + 1), registry_commit), /feed_too_large/);
  assert.throws(() => normalize_feed('{', registry_commit), /invalid_json/);
  const feed = structuredClone(fixture); feed.plugins[0].verifiedCommit = 'main';
  assert.throws(() => normalize(feed), /invalid_commit/);
});
test('allows only the two GitHub endpoints with an immutable feed commit', () => {
  assert.equal(fetch_url_allowed(`https://api.github.com/repos/${source_registry}/commits/main`), true);
  assert.equal(fetch_url_allowed(`https://raw.githubusercontent.com/${source_registry}/${registry_commit}/registry/plugins.json`), true);
  const authenticated = `https://${'user'}:${'fixture'}@api.github.com/repos/${source_registry}/commits/main`;
  for (const url of ['http://github.com/example/a', `https://raw.githubusercontent.com/${source_registry}/main/registry/plugins.json`, `https://api.github.com/repos/${source_registry}/commits/main?token=x`, authenticated]) assert.equal(fetch_url_allowed(url), false);
});
test('scrubs privacy-bearing descriptions with stable tokens and counters', () => {
  const email = ['person', 'company.internal'].join('@');
  const authenticated = `https://${'person'}:${'credential'}@${'service'}.company.internal/path`;
  const windows_path = ['C:', 'Users', 'private_person', 'project'].join('\\');
  const posix_path = ['', 'home', 'private_person', 'project'].join('/');
  const private_address = [10, 45, 67, 89].join('.');
  const tailnet = ['private-node', 'private-mesh', 'ts', 'net'].join('.');
  const source = [email, authenticated, windows_path, posix_path, private_address, tailnet].join(' ');
  const result = privacy_scrub_text(source);
  assert.deepEqual(result.counts, { authenticated_url: 1, email: 1, user_path: 2, private_network: 2 });
  assert.ok(Object.values(privacy_replacement_tokens).every(token => result.value.includes(token)));
  assert.ok(!result.value.includes('private_person'));
});
test('scrubs sentence punctuation and private IPv6 while preserving public IPv6', () => {
  const email = ['person', 'example.test'].join('@');
  const private_ipv4 = [192, 168, 4, 5].join('.');
  const tailnet_ipv6 = ['fd7a', '115c', 'a1e0', '', '42'].join(':');
  const link_local_ipv6 = ['fe80', '', '1'].join(':');
  const public_ipv6 = ['2001', '4860', '4860', '', '8888'].join(':');
  const result = privacy_scrub_text(
    `Contact (${email}). Hosts ${private_ipv4}. [${tailnet_ipv6}]:443 and ${link_local_ipv6}; public ${public_ipv6}.`,
  );
  assert.equal(result.counts.email, 1);
  assert.equal(result.counts.private_network, 3);
  assert.ok(!result.value.includes(email));
  assert.ok(!result.value.includes(private_ipv4));
  assert.ok(!result.value.includes(tailnet_ipv6));
  assert.ok(!result.value.includes(link_local_ipv6));
  assert.ok(result.value.includes(public_ipv6));
});
test('scrubs assignment-prefixed paths, IPs, and IPv4-mapped private IPv6', () => {
  const private_ipv4 = [10, 12, 34, 56].join('.');
  const user_path = ['', 'home', 'fixture-user', 'private-config'].join('/');
  const private_ipv6 = ['fd7a', '115c', 'a1e0', '', '5'].join(':');
  const mapped_ipv6 = `::ffff:${private_ipv4}`;
  const result = privacy_scrub_text(
    `IP=${private_ipv4} PATH=${user_path} V6=${private_ipv6} MAPPED=${mapped_ipv6}`,
  );
  for (const private_value of [private_ipv4, user_path, private_ipv6, mapped_ipv6]) {
    assert.ok(!result.value.includes(private_value));
  }
  assert.equal(result.counts.private_network, 3);
  assert.equal(result.counts.user_path, 1);
});
test('scrubs license display text and rejects unsafe package identities', () => {
  const feed = structuredClone(fixture);
  const email = ['license-owner', 'example.test'].join('@');
  feed.plugins[0].license = `Custom terms: ${email}.`;
  const catalog = normalize(feed);
  assert.equal(catalog.entries[0].license, 'Custom terms: [redacted_email].');
  assert.ok(catalog.privacy_scrub.fields.includes('license'));

  feed.plugins[0].packageName = 'scope/../../private';
  assert.throws(() => normalize(feed), /invalid_package/);
});
test('snapshot sanitization preserves every entry and records offline scrub provenance', () => {
  const catalog = normalize(fixture);
  const email = ['person', 'company.internal'].join('@');
  delete catalog.privacy_scrub;
  catalog.entries[0].description = `Contact ${email}`;
  catalog.entries[0].license = `Owner ${email}`;
  const sanitized = sanitize_catalog_snapshot(catalog);
  assert.equal(sanitized.entry_count, catalog.entry_count);
  assert.deepEqual(sanitized.entries.map(entry => entry.id), catalog.entries.map(entry => entry.id));
  assert.equal(sanitized.privacy_scrub.replacements.email, 2);
  assert.equal(sanitized.privacy_scrub.modified_entries, 1);
  assert.strictEqual(sanitize_catalog_snapshot(sanitized), sanitized);
});
test('bounded fetch rejects oversized streamed responses and requires redirect rejection with no credentials', async () => {
  let request;
  await assert.rejects(fetch_bounded(`https://api.github.com/repos/${source_registry}/commits/main`, {limit:4, fetch_impl:async (url, options) => {
    request = options; return new Response(new Uint8Array(5));
  }}), /feed_too_large/);
  assert.equal(request.redirect, 'error'); assert.equal(request.credentials, 'omit');
  assert.equal(request.headers.authorization, undefined);
});
test('collection resolves a branch once then reads the immutable commit', async () => {
  const requests = [];
  const catalog = await collect_catalog({ fetched_at, fetch_impl:async url => {
    requests.push(url); return new Response(requests.length === 1 ? JSON.stringify({sha:registry_commit}) : JSON.stringify(fixture));
  }});
  assert.equal(requests.length, 2); assert.ok(requests[1].includes(registry_commit));
  assert.equal(catalog.entry_count, 2);
});
test('failed collection retains the last good on-disk catalog; successful writes leave no temporary files', async () => {
  const root = await mkdtemp(join(tmpdir(), 'aag_market_test_'));
  try {
    const target = join(root, 'catalog.json'); await write_catalog(normalize(fixture), target);
    const before = await readFile(target, 'utf8');
    await assert.rejects(async () => {
      const catalog = await collect_catalog({fetch_impl:async () => new Response('', {status:429})});
      await write_catalog(catalog, target);
    }, /registry_rate_limited/);
    assert.equal(await readFile(target, 'utf8'), before);
    assert.deepEqual(await readdir(root), ['catalog.json']);
    await mkdir(join(root, 'blocked'));
    await assert.rejects(write_catalog(normalize(fixture), join(root, 'blocked')));
    assert.deepEqual((await readdir(root)).sort(), ['blocked', 'catalog.json']);
  } finally { await rm(root, {recursive:true, force:true}); }
});
test('bundled snapshot has every record, immutable provenance and bounded descriptions', async () => {
  const catalog = JSON.parse(await readFile(new URL('../../shared/dsh_market_catalog.json', import.meta.url), 'utf8'));
  assert.equal(catalog.entry_count, catalog.entries.length);
  assert.equal(new Set(catalog.entries.map(entry => entry.id)).size, catalog.entry_count);
  assert.ok(catalog.entries.every(entry => Array.from(entry.description).length <= 400));
  assert.ok(catalog.entries.every(entry => Array.isArray(entry.modules) && entry.modules.length > 0));
  assert.ok(catalog.entries.every(entry => entry.classification_basis === 'metadata_rules_v2'));
  assert.match(catalog.registry_commit, /^[a-f0-9]{40}$/);
  assert.equal(catalog.privacy_scrub.schema_version, 1);
  assert.equal(catalog.privacy_scrub.source_entry_count, catalog.entry_count);
  assert.ok(catalog.privacy_scrub.total_replacements > 0);
  assert.ok(Buffer.byteLength(JSON.stringify(catalog)) < max_catalog_bytes);
});

test('rejects invalid UTF-8 rather than silently changing registry metadata', async () => {
  await assert.rejects(fetch_bounded(`https://api.github.com/repos/${source_registry}/commits/main`, {
    fetch_impl: async () => new Response(new Uint8Array([0xff])),
  }), /invalid_utf8/);
});
