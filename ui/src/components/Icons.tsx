/// Reusable inline SVG icons. All icons are stroke-only (currentColor),
/// 16x16 by default. Stroke width 1.7 for that "Apple systemBlue" line-icon
/// feel. No fills, no trademarks — abstract glyphs that hint at each app's
/// metaphor without ripping any vendor's logo.
///
/// `className` is forwarded onto the root <svg> so callers can size with
/// `font-size` (the <svg> uses em-based sizing via width="1em" / height="1em")
/// or override `stroke-width` / `opacity`.

import type { CSSProperties } from 'react';

interface IconProps {
  className?: string;
  style?: CSSProperties;
  title?: string;
}

function svgProps(p: IconProps) {
  return {
    className: p.className,
    style: p.style,
    width: '1em',
    height: '1em',
    viewBox: '0 0 16 16',
    fill: 'none',
    stroke: 'currentColor',
    strokeWidth: 1.7,
    strokeLinecap: 'round' as const,
    strokeLinejoin: 'round' as const,
    role: p.title ? 'img' : 'presentation',
    'aria-label': p.title,
    'aria-hidden': p.title ? undefined : true,
  };
}

/// A spark / 4-point star — abstract stand-in for the Claude brand mark.
/// Pointed tips + a tiny inner diamond at the center for a touch of detail.
export function ClaudeIcon(props: IconProps = {}) {
  return (
    <svg {...svgProps(props)}>
      <path d="M8 1.5 L9.4 6.6 L14.5 8 L9.4 9.4 L8 14.5 L6.6 9.4 L1.5 8 L6.6 6.6 Z" />
      <path d="M8 6 L10 8 L8 10 L6 8 Z" />
    </svg>
  );
}

/// Terminal prompt chevron `>_` — abstract stand-in for Codex / code-agent tools.
/// A simple `>` rendered as an outline chevron, with a cursor block.
export function CodexIcon(props: IconProps = {}) {
  return (
    <svg {...svgProps(props)}>
      <path d="M3 5 L7 8 L3 11" />
      <path d="M8.5 11 L13.5 11" />
      <rect x="8.5" y="5" width="1.5" height="6" rx="0.3" />
    </svg>
  );
}

/// Hub / spokes icon — abstract stand-in for Copilot CLI (a pilot's yoke
/// or a wheel would have been too on-the-nose). Circle + 3 spokes.
export function CopilotIcon(props: IconProps = {}) {
  return (
    <svg {...svgProps(props)}>
      <circle cx="8" cy="8" r="2.2" />
      <circle cx="8" cy="8" r="5.6" />
      <path d="M8 2.4 L8 5.2" />
      <path d="M12.4 11 L10.6 9.2" />
      <path d="M3.6 11 L5.4 9.2" />
    </svg>
  );
}

/// Two overlapping rounded rects — the standard "duplicate" glyph.
export function CopyIcon(props: IconProps = {}) {
  return (
    <svg {...svgProps(props)}>
      <rect x="5" y="5" width="8" height="8" rx="1.5" />
      <path d="M3 11 L3 4 A1 1 0 0 1 4 3 L11 3" />
    </svg>
  );
}

/// Lightning bolt — abstract "test connection" symbol (a beaker would be
/// too literal). Single closed stroke, no fill.
export function TestIcon(props: IconProps = {}) {
  return (
    <svg {...svgProps(props)}>
      <path d="M9 1.5 L4 9 L7.5 9 L6.5 14.5 L12 7 L8.5 7 Z" />
    </svg>
  );
}

/// Small "A" logomark — AAG. Stylised: angled top, single crossbar.
/// Sized for the 24-28px header brand area.
export function BrandLogo(props: IconProps = {}) {
  return (
    <img
      src="/app_icon.svg"
      className={props.className}
      style={props.style}
      width={28}
      height={28}
      alt={props.title ?? ''}
      aria-hidden={props.title ? undefined : true}
    />
  );
}

/// Sun (light theme toggle) — kept here alongside the rest. Same idiom as
/// the original App.tsx local SunIcon/MoonIcon, just centralised.
export function SunIcon(props: IconProps = {}) {
  return (
    <svg {...svgProps(props)} viewBox="0 0 24 24" strokeWidth={2}>
      <circle cx="12" cy="12" r="5" />
      <line x1="12" y1="1" x2="12" y2="3" />
      <line x1="12" y1="21" x2="12" y2="23" />
      <line x1="4.22" y1="4.22" x2="5.64" y2="5.64" />
      <line x1="18.36" y1="18.36" x2="19.78" y2="19.78" />
      <line x1="1" y1="12" x2="3" y2="12" />
      <line x1="21" y1="12" x2="23" y2="12" />
      <line x1="4.22" y1="19.78" x2="5.64" y2="18.36" />
      <line x1="18.36" y1="5.64" x2="19.78" y2="4.22" />
    </svg>
  );
}

/// Moon (dark theme toggle) — same as the original App.tsx.
export function MoonIcon(props: IconProps = {}) {
  return (
    <svg {...svgProps(props)} viewBox="0 0 24 24" strokeWidth={2}>
      <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
    </svg>
  );
}
