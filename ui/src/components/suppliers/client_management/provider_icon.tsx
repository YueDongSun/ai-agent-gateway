import type { ReactNode } from 'react';
import './provider_icon.css';

/** Designed provider icon system: every provider renders as a brand-colored
 *  tile carrying a geometric line glyph (white, 24×24 grid, 2px stroke,
 *  round caps) instead of the old emoji-in-a-box. The glyphs are abstract
 *  marks keyed by `ProviderPresetTemplate.icon`, so that field is now a
 *  glyph id when it matches one here; any other non-empty value (a letter
 *  or emoji the user typed in the modal) still renders as a literal
 *  character on the tile. Cards with no icon fall back to an initials
 *  monogram on a deterministic hue so every provider gets a tile. */

/** Snake_case glyph ids, one per brand family (region variants share the
 *  family mark and are told apart by name and tile color). */
export type ProviderGlyph =
  | 'spark'
  | 'nodes'
  | 'moon'
  | 'waves'
  | 'whale'
  | 'cross'
  | 'steps'
  | 'chip'
  | 'sail'
  | 'cloud'
  | 'petals'
  | 'volcano'
  | 'gem'
  | 'llama'
  | 'branch'
  | 'flask'
  | 'pipe'
  | 'robot'
  | 'cherry'
  | 'lotus'
  | 'motion'
  | 'keyboard'
  | 'fish'
  | 'apple'
  | 'gear'
  | 'knot'
  | 'hub';

const GLYPH_IDS: ProviderGlyph[] = [
  'spark',
  'nodes',
  'moon',
  'waves',
  'whale',
  'cross',
  'steps',
  'chip',
  'sail',
  'cloud',
  'petals',
  'volcano',
  'gem',
  'llama',
  'branch',
  'flask',
  'pipe',
  'robot',
  'cherry',
  'lotus',
  'motion',
  'keyboard',
  'fish',
  'apple',
  'gear',
  'knot',
  'hub',
];

/** Glyph bodies for the shared 24×24 stroke wrapper. Tiny filled circles
 *  (eye dots, key caps) use fill=currentColor / stroke=none inline. */
const GLYPH_BODIES: Record<ProviderGlyph, ReactNode> = {
  // Anthropic: six-spoke spark (the ✳ mark, drawn).
  spark: (
    <>
      <path d="M12 3.5v17" />
      <path d="M4.6 7.75l14.8 8.5" />
      <path d="M4.6 16.25l14.8-8.5" />
    </>
  ),
  // GLM / Zhipu: neural triad.
  nodes: (
    <>
      <circle cx="12" cy="5.6" r="2.2" />
      <circle cx="6.4" cy="16.4" r="2.2" />
      <circle cx="17.6" cy="16.4" r="2.2" />
      <path d="M10.4 7.4L7.8 14.5" />
      <path d="M13.6 7.4l2.6 7.1" />
      <path d="M8.6 16.4h6.8" />
    </>
  ),
  // Kimi / Moonshot: crescent.
  moon: <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />,
  // MiniMax: stacked swells.
  waves: (
    <>
      <path d="M3.5 9c2.7-2.8 5.3-2.8 8 0s5.3 2.8 8 0" />
      <path d="M3.5 15c2.7-2.8 5.3-2.8 8 0s5.3 2.8 8 0" />
    </>
  ),
  // DeepSeek: whale contour with a tail flick.
  whale: (
    <>
      <path d="M4.5 13.5c1.8-4.8 6.4-7.5 11-6.3 2.2.6 4 2.1 5 4.1" />
      <path d="M4.5 13.5c.6 3 3 5.2 6 5.6 2.7.4 5.3-.7 6.8-2.6" />
      <path d="M20.5 11.3l1.8-3M20.5 11.3l2.8 1.2" />
      <circle cx="8.4" cy="12.8" r="1" fill="currentColor" stroke="none" />
    </>
  ),
  // xAI Grok: cross strokes.
  cross: (
    <>
      <path d="M6 6l12 12" />
      <path d="M18 6L6 18" />
    </>
  ),
  // StepFun: ascending steps.
  steps: <path d="M4 18.5H8.5V14H13V9.5H17.5V5" />,
  // Xiaomi MiMo: chip with pins.
  chip: (
    <>
      <rect x="5.5" y="5.5" width="13" height="13" rx="2.5" />
      <path d="M9.5 2.2v3.3M14.5 2.2v3.3M9.5 18.5v3.3M14.5 18.5v3.3" />
      <circle cx="12" cy="12" r="1.8" fill="currentColor" stroke="none" />
    </>
  ),
  // Baidu Qianfan (千帆, thousand sails): two sails over water.
  sail: (
    <>
      <path d="M12 3.8L5.2 16.3H12z" />
      <path d="M13 10l5.8 6.3H13z" />
      <path d="M3.5 19.5c2.2-1.7 4.3-1.7 6.5 0s4.3 1.7 6.5 0" />
    </>
  ),
  // Tencent Cloud: cloud outline.
  cloud: <path d="M18 10h-1.26A8 8 0 1 0 9 20h9a5 5 0 0 0 0-10z" />,
  // Huawei: five-petal rosette.
  petals: (
    <>
      <circle cx="12" cy="6.6" r="2.7" />
      <circle cx="17.1" cy="10.3" r="2.7" />
      <circle cx="15.2" cy="16.4" r="2.7" />
      <circle cx="8.8" cy="16.4" r="2.7" />
      <circle cx="6.9" cy="10.3" r="2.7" />
      <circle cx="12" cy="12.2" r="1.1" fill="currentColor" stroke="none" />
    </>
  ),
  // Volcengine Ark: erupting cone.
  volcano: (
    <>
      <path d="M3.8 19.5L9.2 9.3l2.3 3.4 2.2-3.8 5.5 10.6H3.8z" />
      <path d="M12 5.5V3" />
      <path d="M9.6 4.2l1 1.5" />
      <path d="M14.4 4.2l-1 1.5" />
    </>
  ),
  // Qwen: faceted gem.
  gem: (
    <>
      <path d="M12 3.8l7.2 4.6v7.2L12 20.2l-7.2-4.6V8.4z" />
      <path d="M4.8 8.4h14.4" />
      <path d="M12 3.8v4.6" />
    </>
  ),
  // Ollama: llama head.
  llama: (
    <>
      <path d="M7.2 13.8c0-3.5 2.2-6 4.8-6s4.8 2.5 4.8 6v1.8c0 2.2-2.1 3.9-4.8 3.9s-4.8-1.7-4.8-3.9z" />
      <path d="M8.4 8.4L6.6 4.2l3.6 2.2" />
      <path d="M15.6 8.4l1.8-4.2-3.6 2.2" />
      <circle cx="10.1" cy="13" r="0.9" fill="currentColor" stroke="none" />
      <circle cx="13.9" cy="13" r="0.9" fill="currentColor" stroke="none" />
      <path d="M10.8 17h2.4" />
    </>
  ),
  // OpenRouter: one request branching to many vendors.
  branch: (
    <>
      <path d="M4 12h5l3-6h5.2" />
      <path d="M4 12h5l3 6h5.2" />
      <path d="M15 4.2L17.2 6 15 7.8" />
      <path d="M15 14.2L17.2 16 15 17.8" />
    </>
  ),
  // AiHubMix: flask with bubbles.
  flask: (
    <>
      <path d="M9.8 3.5h4.4" />
      <path d="M10.4 3.5v5.2L5.8 17.8a2 2 0 0 0 1.8 3h8.8a2 2 0 0 0 1.8-3l-4.6-9.1V3.5" />
      <circle cx="10.2" cy="13.8" r="0.9" fill="currentColor" stroke="none" />
      <circle cx="13.6" cy="16.2" r="0.7" fill="currentColor" stroke="none" />
    </>
  ),
  // PipeLLM: perforated pipe.
  pipe: (
    <>
      <rect x="8.8" y="3.2" width="6.4" height="17.6" rx="3.2" />
      <circle cx="12" cy="8" r="1" fill="currentColor" stroke="none" />
      <circle cx="12" cy="12" r="1" fill="currentColor" stroke="none" />
      <circle cx="12" cy="16" r="1" fill="currentColor" stroke="none" />
    </>
  ),
  // 302.AI: robot head.
  robot: (
    <>
      <rect x="5" y="8.2" width="14" height="10.6" rx="2.4" />
      <path d="M12 8.2V5" />
      <circle cx="12" cy="3.6" r="1.3" fill="currentColor" stroke="none" />
      <circle cx="9.6" cy="13" r="1.1" fill="currentColor" stroke="none" />
      <circle cx="14.4" cy="13" r="1.1" fill="currentColor" stroke="none" />
      <path d="M9.8 16.4h4.4" />
    </>
  ),
  // CherryIN: paired cherries.
  cherry: (
    <>
      <circle cx="8.6" cy="15.8" r="3.1" />
      <circle cx="16.2" cy="16.8" r="2.6" />
      <path d="M8.6 12.7c.2-4 2.2-6.9 5.2-8.4" />
      <path d="M16.2 14.2c-.3-3.4.7-6.1 2.6-8" />
      <path d="M13.8 4.3c1.6-.9 3.2-.8 4.7.2" />
    </>
  ),
  // OpenCode Zen: enso brush circle with a center dot.
  lotus: (
    <>
      <path d="M12 4.2a7.8 7.8 0 1 1-5.5 13.3" />
      <circle cx="12" cy="12" r="1.6" fill="currentColor" stroke="none" />
    </>
  ),
  // OpenCode Go: motion chevrons.
  motion: (
    <>
      <path d="M6.5 5.8l6.2 6.2-6.2 6.2" />
      <path d="M13.5 5.8l6.2 6.2-6.2 6.2" />
    </>
  ),
  // Command Code: keycaps.
  keyboard: (
    <>
      <rect x="3" y="6.5" width="18" height="11" rx="2.2" />
      <path d="M7 10h.01M10.3 10h.01M13.7 10h.01M17 10h.01" />
      <path d="M8 14h8" />
    </>
  ),
  // yylx (鱼鱼连线): fish with tail.
  fish: (
    <>
      <path d="M20.5 12c-2.6-3.6-6.2-5.3-9.4-5.3-4.2 0-7.3 2.3-8.6 5.3 1.3 3 4.4 5.3 8.6 5.3 3.2 0 6.8-1.7 9.4-5.3z" />
      <path d="M20.5 12l2.6-3v6z" />
      <circle cx="8.2" cy="11" r="1" fill="currentColor" stroke="none" />
    </>
  ),
  // oMLX: apple with stem.
  apple: (
    <>
      <path d="M12 8.2c-1.9-2.4-5.7-2-7.2 1-1.6 3.2.6 8.4 3.6 10.8 1.4 1.1 2.6 1 3.6 0 1 1 2.2 1.1 3.6 0 3-2.4 5.2-7.6 3.6-10.8-1.5-3-5.3-3.4-7.2-1z" />
      <path d="M12 8.2c0-1.9 1.4-3.5 3.4-4.2" />
    </>
  ),
  // MLX-Serve: gear.
  gear: (
    <>
      <circle cx="12" cy="12" r="3.2" />
      <path d="M12 2.8v2.6M12 18.6v2.6M2.8 12h2.6M18.6 12h2.6M5.4 5.4l1.9 1.9M16.7 16.7l1.9 1.9M5.4 18.6l1.9-1.9M16.7 7.3l1.9-1.9" />
    </>
  ),
  // OpenAI-family brands: six-arc knot blossom.
  knot: (
    <>
      <ellipse cx="12" cy="12" rx="3.2" ry="8" transform="rotate(0 12 12)" />
      <ellipse cx="12" cy="12" rx="3.2" ry="8" transform="rotate(60 12 12)" />
      <ellipse cx="12" cy="12" rx="3.2" ry="8" transform="rotate(120 12 12)" />
    </>
  ),
  // AAG gateway rows: distribution hub (the failover centerpiece).
  hub: (
    <>
      <circle cx="12" cy="12" r="2" fill="currentColor" stroke="none" />
      <path d="M12 10V5.7M12 14v4.3M10 12H5.7M14 12h4.3" />
      <circle cx="12" cy="4.4" r="1.3" fill="currentColor" stroke="none" />
      <circle cx="12" cy="19.6" r="1.3" fill="currentColor" stroke="none" />
      <circle cx="4.4" cy="12" r="1.3" fill="currentColor" stroke="none" />
      <circle cx="19.6" cy="12" r="1.3" fill="currentColor" stroke="none" />
    </>
  ),
};

/** True when `icon` names a designed glyph rather than a literal char. */
export function is_provider_glyph(icon: string): icon is ProviderGlyph {
  return (GLYPH_IDS as string[]).includes(icon);
}

/** Every glyph id, for the modal's icon-input datalist. */
export function provider_glyph_ids(): string[] {
  return [...GLYPH_IDS];
}

/** Initials for the monogram fallback: up to two letters of the name. */
function initials_of(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  if (parts.length === 0) return '?';
  return (parts[0][0] + (parts[1]?.[0] ?? '')).toUpperCase();
}

/** Deterministic tile hue for name-only monograms: same name, same color
 *  across sessions and machines. */
const MONOGRAM_HUES = [20, 210, 170, 265, 330, 30, 225, 190, 95, 15];

function monogram_hue(name: string): string {
  let hash = 0;
  for (let i = 0; i < name.length; i += 1) {
    hash = (hash * 31 + name.charCodeAt(i)) | 0;
  }
  return `hsl(${MONOGRAM_HUES[Math.abs(hash) % MONOGRAM_HUES.length]} 58% 48%)`;
}

export interface ProviderIconProps {
  /** Glyph id, or a literal character (letter/emoji) the user set. */
  icon?: string | null;
  /** Tile background; defaults to a deterministic monogram hue. */
  color?: string | null;
  /** Provider display name — drives the monogram fallback. */
  name: string;
  /** Tile size: 28px picker rows, 32px cards and modal preview. */
  size?: 'sm' | 'md';
  className?: string;
}

/** The shared provider tile: brand-colored squircle, soft top-light sheen,
 *  and (in order) a designed glyph, a user character, or an initials
 *  monogram. */
export function Provider_icon({
  icon,
  color,
  name,
  size = 'md',
  className,
}: ProviderIconProps) {
  const trimmed = icon?.trim() ?? '';
  const glyph = is_provider_glyph(trimmed) ? trimmed : null;
  const char = glyph ? '' : trimmed;
  const background = color?.trim() || monogram_hue(name);
  const classes = ['provider-icon', `provider-icon-${size}`, className]
    .filter(Boolean)
    .join(' ');

  return (
    <span
      className={classes}
      style={{ background }}
      aria-hidden
      data-testid="provider-icon"
    >
      {glyph ? (
        <svg
          className="provider-icon-glyph"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth={2}
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          {GLYPH_BODIES[glyph]}
        </svg>
      ) : (
        <span className="provider-icon-char">{char || initials_of(name)}</span>
      )}
    </span>
  );
}
