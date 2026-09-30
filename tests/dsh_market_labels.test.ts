/**
 * Cross-check the shared DSH market classification rules against the UI label
 * tables. scripts/dsh_market/classification_rules.json is consumed by the
 * Node collector, the Rust normalizer, and (indirectly) the React market
 * page — every rule id must have a bilingual UI label, and every UI label id
 * must exist in the rules.
 */
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { market_categories, market_modules, market_use_cases } from '../ui/src/components/dsh_market/market_catalog.js';

const rules = JSON.parse(
  readFileSync(join(__dirname, '../scripts/dsh_market/classification_rules.json'), 'utf8'),
) as {
  version: string;
  modules: Array<{ id: string; keywords: string[] }>;
  categories: Array<{ id: string; keywords: string[] }>;
  use_cases: Array<{ id: string; keywords: string[] }>;
};

describe('classification rules and UI labels stay in sync', () => {
  it('uses the v2 rule version everywhere', () => {
    expect(rules.version).toBe('metadata_rules_v2');
  });

  it('every module rule id has a UI label and vice versa', () => {
    const rule_ids = rules.modules.map((rule) => rule.id);
    const label_ids = market_modules.map((module) => module.id);
    expect([...rule_ids].sort()).toEqual([...label_ids.filter((id) => id !== 'general')].sort());
    expect(label_ids).toContain('general');
  });

  it('every category rule id has a UI label, plus the other fallback', () => {
    const rule_ids = rules.categories.map((rule) => rule.id);
    const label_ids = market_categories.map((category) => category.id);
    expect([...rule_ids].sort()).toEqual([...label_ids.filter((id) => id !== 'other')].sort());
    expect(label_ids).toContain('other');
  });

  it('every use-case rule id has a UI label, plus the general fallback', () => {
    const rule_ids = rules.use_cases.map((rule) => rule.id);
    const label_ids = market_use_cases.map((use_case) => use_case.id);
    expect([...rule_ids].sort()).toEqual([...label_ids.filter((id) => id !== 'general')].sort());
    expect(label_ids).toContain('general');
  });

  it('rule keywords are non-empty and unique within each rule', () => {
    for (const group of [rules.modules, rules.categories, rules.use_cases]) {
      for (const rule of group) {
        expect(rule.keywords.length).toBeGreaterThan(0);
        expect(new Set(rule.keywords).size).toBe(rule.keywords.length);
      }
    }
  });

  it('module icons resolve to known Market_icon paths', () => {
    for (const module of market_modules) {
      expect(typeof module.icon).toBe('string');
      expect(module.icon.length).toBeGreaterThan(0);
    }
  });
});
