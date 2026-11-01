/// The i18n README requires every key to exist in both catalogs, and
/// documents `{var}` interpolation. Nothing enforced either: a key added
/// to only one file silently fell back to English (dev) or to the raw key
/// (release), and a message that dropped a placeholder rendered the
/// literal `{name}` in the UI.
import { describe, expect, it } from 'vitest';
import { en } from '../ui/src/i18n/en.js';
import { zh } from '../ui/src/i18n/zh.js';

/** Interpolation placeholders used by a message, in sorted order. */
function placeholders(message: string): string[] {
  return [...message.matchAll(/\{(\w+)\}/g)].map((match) => match[1]).sort();
}

describe('i18n catalogs', () => {
  const en_keys = Object.keys(en);
  const zh_keys = Object.keys(zh);

  it('define the same key set', () => {
    expect(zh_keys.filter((key) => !(key in en))).toEqual([]);
    expect(en_keys.filter((key) => !(key in zh))).toEqual([]);
  });

  it('interpolate the same placeholders per key', () => {
    const mismatched = en_keys.filter((key) => {
      const translated = zh[key as keyof typeof zh];
      return (
        translated !== undefined &&
        placeholders(translated).join(',') !== placeholders(en[key]).join(',')
      );
    });
    expect(mismatched).toEqual([]);
  });
});
