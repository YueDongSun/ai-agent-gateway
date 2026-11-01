import type { Arena_point } from './api/arena_visualization.js';

const geometry_config = { bins: 28, sample_limit: 72, plot_left: 60, plot_width: 680 };

/** A resolved runtime primary supersedes the saved selector's assessment.
 * Missing evidence on that primary stays missing; later hops cannot supply it. */
export function arena_selected_assessment(snapshot: {
  planSource: string; selected: Arena_point | null; candidates: readonly { arena: Arena_point | null }[];
} | null): Arena_point | null {
  if (!snapshot) return null;
  return snapshot.planSource === 'admitted_runtime' && snapshot.candidates.length > 0
    ? snapshot.candidates[0].arena : snapshot.selected;
}

export function arena_gap(left: Arena_point | null, right: Arena_point | null) {
  if (!left || !right) return null;
  return { difference: left.rating - right.rating,
    intervals_overlap: left.ratingLower <= right.ratingUpper && right.ratingLower <= left.ratingUpper };
}
export function arena_geometry(entries: Arena_point[]) {
  const valid = entries.filter((entry) => [entry.rating, entry.ratingLower, entry.ratingUpper].every(Number.isFinite)
    && entry.ratingLower <= entry.rating && entry.rating <= entry.ratingUpper);
  if (!valid.length) return null;
  const low = Math.min(...valid.map((entry) => entry.ratingLower));
  const high = Math.max(...valid.map((entry) => entry.ratingUpper));
  const span = Math.max(1, high - low);
  const x = (score: number) => geometry_config.plot_left + (score - low) / span * geometry_config.plot_width;
  const bins = Array.from({ length: geometry_config.bins }, () => 0);
  for (const entry of valid) bins[Math.min(bins.length - 1, Math.max(0, Math.floor((entry.rating - low) / span * bins.length)))]++;
  const step = Math.max(1, Math.ceil(valid.length / geometry_config.sample_limit));
  return { low, high, x, bins, sample: valid.filter((_, index) => index % step === 0), count: valid.length };
}
