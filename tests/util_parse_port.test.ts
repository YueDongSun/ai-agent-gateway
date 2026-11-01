import { describe, expect, it } from 'vitest';
import { parse_port_input } from '../src/util/parse_port.js';

describe('parse_port_input', () => {
  it.each(['8787', '1', '65535', ' 8989 '])('accepts %s', (raw) => {
    expect(parse_port_input(raw)).toBe(Number(raw.trim()));
  });

  it.each(['abc', '', '0', '-1', '99999', '8989.5', '8787abc'])('rejects %s', (raw) => {
    expect(parse_port_input(raw)).toBeNull();
  });
});
