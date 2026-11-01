import { describe, expect, it } from 'vitest';
import { blend_rgb, contrast_ratio, parse_color, resolve_status_surface } from '../ui/src/lib/status_surface.js';

describe('adaptive status surface', () => {
  it('keeps status text AA and status icons AA over any backdrop extremes', () => {
    const themes = [
      { base: '#faf9f5', text: '#302f2c', icons: ['#4e7455', '#b45309'] },
      { base: '#24231f', text: '#eeeae1', icons: ['#8eaf88', '#fbbf24'] },
    ];
    for (const theme of themes) {
      for (const sample of ['#000000', '#ffffff', '#d18466', '#426c4d']) {
        for (const scroll of [0, 12, 48, 96, 5000]) {
          const surface = resolve_status_surface(parse_color(theme.base)!, parse_color(sample)!, scroll);
          for (const backdrop of ['#000000', '#ffffff']) {
            const visible = blend_rgb(parse_color(backdrop)!, surface.color, surface.opacity);
            expect(contrast_ratio(parse_color(theme.text)!, visible)).toBeGreaterThanOrEqual(4.5);
            for (const icon of theme.icons) expect(contrast_ratio(parse_color(icon)!, visible)).toBeGreaterThanOrEqual(3);
          }
        }
      }
    }
  });
  it('starts with the page surface and progressively echoes content without an opaque scroll threshold', () => {
    const base = parse_color('#faf9f5')!;
    const sample = parse_color('#d18466')!;
    expect(resolve_status_surface(base, sample, 0).color).toEqual(base);
    expect(resolve_status_surface(base, sample, 24).color).not.toEqual(base);
    expect(resolve_status_surface(base, sample, 96).color).not.toEqual(sample);
    expect(resolve_status_surface(base, sample, -100)).toEqual(resolve_status_surface(base, sample, 0));
  });
  it('parses browser colors and rejects unsupported values without guessing', () => {
    expect(parse_color('rgb(250, 249, 245)')).toEqual([250, 249, 245]);
    expect(parse_color('rgba(50, 40, 30, 0.2)')).toEqual([50, 40, 30]);
    expect(parse_color('transparent')).toBeNull();
    expect(parse_color('var(--bg)')).toBeNull();
  });
});
