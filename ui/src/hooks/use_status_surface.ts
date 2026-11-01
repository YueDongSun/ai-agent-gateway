import { useLayoutEffect, type RefObject } from 'react';
import { blend_rgb, parse_color, resolve_status_surface, type Rgb } from '../lib/status_surface.js';

/** Composite computed backgrounds from the page outward, excluding the header
 * and floating dialogs. No canvas, screenshots, content text, or pixel readback. */
function page_color_at(x: number, y: number, main: HTMLElement, base: Rgb): Rgb {
  const target = document.elementsFromPoint(x, y).find((element) =>
    main.contains(element) && !element.closest('[role="dialog"], .modal-backdrop'));
  const layers: { color: Rgb; alpha: number }[] = [];
  let element = target;
  while (element && main.contains(element)) {
    const value = getComputedStyle(element).backgroundColor;
    const color = parse_color(value);
    const alpha = value.startsWith('rgba') ? Number(value.match(/,\s*([\d.]+)\)$/)?.[1] ?? 1) : 1;
    if (color && alpha > 0) layers.push({ color, alpha });
    element = element.parentElement ?? undefined;
  }
  return layers.reverse().reduce((under, layer) => blend_rgb(under, layer.color, layer.alpha), base);
}

export function use_status_surface(header_ref: RefObject<HTMLElement>, main_ref: RefObject<HTMLElement>, route: string, theme: string) {
  useLayoutEffect(() => {
    const header = header_ref.current;
    const main = main_ref.current;
    if (!header || !main) return;
    let frame = 0;
    let settle_frame = 0;
    const base = parse_color(getComputedStyle(document.documentElement).getPropertyValue('--bg').trim());
    if (!base) return;

    const sample = () => {
      frame = 0;
      const bounds = header.getBoundingClientRect();
      const y = Math.max(1, Math.min(innerHeight - 1, bounds.top + bounds.height / 2));
      const colors = [0.2, 0.5, 0.8].map((fraction) => page_color_at(bounds.left + bounds.width * fraction, y, main, base));
      const color = colors.reduce((sum, value) => sum.map((channel, index) => channel + value[index] / colors.length) as Rgb, [0, 0, 0] as Rgb);
      const surface = resolve_status_surface(base, color, window.scrollY);
      header.style.setProperty('--status_rgb', surface.color.map((channel) => channel.toFixed(2)).join(', '));
      header.style.setProperty('--status_opacity', surface.opacity.toFixed(3));
      header.style.setProperty('--status_progress', surface.progress.toFixed(3));
      document.documentElement.style.setProperty('--workspace_header_height', `${bounds.height}px`);
    };
    const schedule = () => { if (!frame) frame = requestAnimationFrame(sample); };
    // A theme change is atomic with its text palette; never tween dark text
    // across the previous dark background (or the inverse).
    header.dataset.surfaceSettling = 'true';
    sample();
    settle_frame = requestAnimationFrame(() => {
      settle_frame = requestAnimationFrame(() => { delete header.dataset.surfaceSettling; });
    });
    window.addEventListener('scroll', schedule, { passive: true });
    window.addEventListener('resize', schedule, { passive: true });
    const resize = new ResizeObserver(schedule);
    resize.observe(header);
    resize.observe(main);
    // Lazy pages and expanded cards can change the sampled surface while idle.
    const content = new MutationObserver(schedule);
    content.observe(main, { childList: true, subtree: true, attributes: true, attributeFilter: ['class', 'hidden', 'open'] });
    return () => {
      window.removeEventListener('scroll', schedule);
      window.removeEventListener('resize', schedule);
      resize.disconnect();
      content.disconnect();
      cancelAnimationFrame(frame);
      cancelAnimationFrame(settle_frame);
    };
  }, [header_ref, main_ref, route, theme]);
}
