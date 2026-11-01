/** A bounded echo of page colors keeps the established foreground readable,
 * including over the darkest/lightest possible content behind the glass. */
export type Rgb = [number, number, number];

const surface_config = { scroll_distance: 96, echo_limit: 0.08, opacity_min: 0.92, opacity_max: 0.96 };

export function parse_color(value: string): Rgb | null {
  const hex = value.trim().match(/^#([\da-f]{6})$/i);
  if (hex) return [0, 2, 4].map((offset) => parseInt(hex[1].slice(offset, offset + 2), 16)) as Rgb;
  const rgb = value.match(/^rgba?\(\s*([\d.]+)[,\s]+([\d.]+)[,\s]+([\d.]+)/);
  return rgb ? rgb.slice(1, 4).map(Number) as Rgb : null;
}

export function blend_rgb(base: Rgb, echo: Rgb, amount: number): Rgb {
  return base.map((channel, index) => channel + (echo[index] - channel) * amount) as Rgb;
}

export function contrast_ratio(foreground: Rgb, background: Rgb): number {
  const luminance = (color: Rgb) => color.reduce((sum, channel, index) => {
    const unit = channel / 255;
    return sum + (unit <= 0.04045 ? unit / 12.92 : ((unit + 0.055) / 1.055) ** 2.4) * [0.2126, 0.7152, 0.0722][index];
  }, 0);
  const values = [luminance(foreground), luminance(background)].sort((a, b) => b - a);
  return (values[0] + 0.05) / (values[1] + 0.05);
}

export function resolve_status_surface(base: Rgb, sample: Rgb, scroll: number) {
  const progress = Math.min(1, Math.max(0, scroll) / surface_config.scroll_distance);
  return {
    color: blend_rgb(base, sample, progress * surface_config.echo_limit),
    opacity: surface_config.opacity_min + (surface_config.opacity_max - surface_config.opacity_min) * progress,
    progress,
  };
}
