import { describe, expect, it } from 'vitest';
import { arena_gap, arena_geometry, arena_selected_assessment } from '../ui/src/lib/arena_geometry.js';

const points = [
  { modelName: 'A', rating: 1400, ratingLower: 1380, ratingUpper: 1420, rank: 1, voteCount: 100 },
  { modelName: 'B', rating: 1300, ratingLower: 1250, ratingUpper: 1350, rank: 2, voteCount: 90 },
];
describe('Arena coordinate evidence', () => {
  it('retains the full distribution when visual markers are sampled', () => {
    const geometry = arena_geometry(points)!;
    expect(geometry.count).toBe(2);
    expect(geometry.bins.reduce((sum, count) => sum + count, 0)).toBe(2);
    expect(geometry.x(1250)).toBe(60);
    expect(geometry.x(1420)).toBe(740);
  });
  it('reports score gaps and overlapping uncertainty without claiming a capability gain', () => {
    expect(arena_gap(points[0], points[1])).toEqual({ difference: 100, intervals_overlap: false });
    expect(arena_gap(points[0], { ...points[1], ratingUpper: 1400 })).toEqual({ difference: 100, intervals_overlap: true });
    expect(arena_gap(null, points[1])).toBeNull();
  });
  it('never assigns coordinates to missing or invalid ratings', () => {
    expect(arena_geometry([])).toBeNull();
    expect(arena_geometry([{ ...points[0], rating: NaN }])).toBeNull();
  });
  it('plots the rated runtime primary even when the configured route alias has no rating', () => {
    expect(arena_selected_assessment({ planSource: 'admitted_runtime', selected: null, candidates: [{ arena: points[1] }, { arena: points[0] }] })).toEqual(points[1]);
  });
  it('does not carry a saved rating onto an unrated runtime primary', () => {
    expect(arena_selected_assessment({ planSource: 'admitted_runtime', selected: points[0], candidates: [{ arena: null }, { arena: points[1] }] })).toBeNull();
  });
  it('retains the configured selection for a saved-only preview', () => {
    expect(arena_selected_assessment({ planSource: 'saved_configuration', selected: points[0], candidates: [{ arena: points[1] }] })).toEqual(points[0]);
    expect(arena_selected_assessment(null)).toBeNull();
  });
});
