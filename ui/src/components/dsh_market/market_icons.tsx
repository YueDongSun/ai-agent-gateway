import type { CSSProperties } from 'react';

const paths: Record<string, JSX.Element> = {
  grid: <><rect x="3" y="3" width="7" height="7" rx="1.5" /><rect x="14" y="3" width="7" height="7" rx="1.5" /><rect x="3" y="14" width="7" height="7" rx="1.5" /><rect x="14" y="14" width="7" height="7" rx="1.5" /></>,
  search: <><circle cx="10.8" cy="10.8" r="6.8" /><path d="m16 16 4.5 4.5" /></>,
  arrow: <><path d="M5 12h14m-5-5 5 5-5 5" /></>,
  external: <><path d="M14 3h7v7m0-7L10 14M10 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-5" /></>,
  star: <path d="m12 3 2.8 5.7 6.3.9-4.6 4.4 1.1 6.3L12 17.4l-5.6 2.9 1.1-6.3L3 9.6l6.2-.9Z" />,
  close: <path d="m6 6 12 12M6 18 18 6" />,
  sun: <><circle cx="12" cy="12" r="4" /><path d="M12 2v2m0 16v2M2 12h2m16 0h2M5 5l1.4 1.4m11.2 11.2L19 19M5 19l1.4-1.4M17.6 6.4 19 5" /></>,
  moon: <path d="M20.5 13.2A8.8 8.8 0 0 1 10.8 3.5a9 9 0 1 0 9.7 9.7Z" />,
  refresh: <><path d="M20 7v5h-5M4 17v-5h5" /><path d="M5.2 7A8 8 0 0 1 19 6l1 6M4 12l1 6a8 8 0 0 0 13.8-1" /></>,
  code: <><path d="m8 7-5 5 5 5m8-10 5 5-5 5m-3-13-2 20" /></>,
  palette: <><path d="M12 3a9 9 0 1 0 0 18h1a2 2 0 0 0 1-3.7c-.8-.5-.4-1.8.5-1.8H17A4 4 0 0 0 21 11a9 9 0 0 0-9-8Z" /><path d="M6.5 11h.1M9 6.8h.1m5 .2h.1m3.3 4h.1" strokeWidth="3" /></>,
  plug: <><path d="M8 3v5m8-5v5M6 8h12v3a6 6 0 0 1-12 0Zm6 9v4" /></>,
  tools: <><path d="m14.5 6.5 3 3 3.5-3.5a7 7 0 0 1-9 9L5.5 21 3 18.5 9 12a7 7 0 0 1 9-9Z" /></>,
  message: <><path d="M21 11a8 8 0 0 1-8 8H7l-4 3V11a8 8 0 0 1 8-8h2a8 8 0 0 1 8 8Z" /><path d="M7 9h10M7 13h6" /></>,
  monitor: <><rect x="3" y="3" width="18" height="13" rx="2" /><path d="M8 21h8m-4-5v5" /></>,
  shield: <><path d="m12 3 8 3v5c0 5-8 10-8 10S4 16 4 11V6Z" /><path d="M12 8v5m0 3h.01" /></>,
  memory: <><rect x="6" y="6" width="12" height="12" rx="3" /><path d="M9 2v4m6-4v4M9 18v4m6-4v4M2 9h4m-4 6h4m12-6h4m-4 6h4M10 10h4v4h-4Z" /></>,
  workflow: <><rect x="8" y="2" width="8" height="6" rx="1" /><rect x="2" y="16" width="8" height="6" rx="1" /><rect x="14" y="16" width="8" height="6" rx="1" /><path d="M12 8v4M6 16v-4h12v4" /></>,
  copy: <><rect x="8" y="8" width="13" height="13" rx="2" /><path d="M16 8V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h3" /></>,
  book: <><path d="M12 5c-3-3-7-3-10-1v15c3-2 7-2 10 1 3-3 7-3 10-1V4c-3-2-7-2-10 1Zm0 0v15" /></>,
  brain: <><path d="M9.5 3a2.5 2.5 0 0 0-2.5 2.5c-1.9.3-3.3 2-3.3 4 0 .6.1 1.1.4 1.6A3.5 3.5 0 0 0 6 17.6a2.5 2.5 0 0 0 5 .9V4.5A2.5 2.5 0 0 0 9.5 3Z" /><path d="M14.5 3a2.5 2.5 0 0 1 2.5 2.5c1.9.3 3.3 2 3.3 4 0 .6-.1 1.1-.4 1.6a3.5 3.5 0 0 1-1.9 6.5 2.5 2.5 0 0 1-5 .9V4.5A2.5 2.5 0 0 1 14.5 3Z" /></>,
  image: <><rect x="3" y="4" width="18" height="16" rx="2" /><circle cx="9" cy="10" r="2" /><path d="m5 19 5-5 3 3 4-4 4 4" /></>,
};

export function Market_icon({ name, class_name, style }: { name: string; class_name?: string; style?: CSSProperties }) {
  return <svg className={class_name} style={style} width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{paths[name] ?? paths.plug}</svg>;
}
