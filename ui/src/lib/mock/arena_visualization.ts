import type { Arena_snapshot, Arena_point, Arena_flow_model } from '../api/arena_visualization.js';
import type { Model_arena_assessment } from '../api/suppliers_logic.js';

const demo_points: Arena_point[] = [
  { modelName: 'demo-strong', rating: 1500, ratingLower: 1480, ratingUpper: 1520, rank: 1, voteCount: 1000 },
  { modelName: 'demo-current', rating: 1450, ratingLower: 1430, ratingUpper: 1470, rank: 2, voteCount: 800 },
  { modelName: 'demo-balanced', rating: 1380, ratingLower: 1350, ratingUpper: 1410, rank: 3, voteCount: 600 },
  { modelName: 'demo-fast', rating: 1280, ratingLower: 1240, ratingUpper: 1320, rank: 4, voteCount: 500 },
];
function assessment(point: Arena_point): Model_arena_assessment {
  return { ...point, publishedDate: '2026-10-20', category: 'overall', sourceId: 'demo-synthetic', sourceUrl: 'https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset', sourceConfig: 'text_style_control', sourceSplit: 'latest', policy: 'demo', condition: null, conditionAmbiguous: false, tier: 3 - point.rank + 1, totalModels: 4, comparedOfferedModels: 4 };
}
export function mock_arena_snapshot(args: Record<string, unknown>): Arena_snapshot {
  const selector = typeof args.selector === 'string' ? args.selector : 'demo-current';
  const selected_model = selector.startsWith('alias:') ? 'demo-strong' : selector.replace(/^precise:/, '');
  const selected = demo_points.find((point) => point.modelName === selected_model);
  const flow = (point: Arena_point): Arena_flow_model => ({ model: point.modelName, provider: `demo-provider-${point.rank}`, protocol: 'openai',
    health: { providerState: null, samples: 0, successRate: null, latencyMs: null, unavailable: false }, cost: null,
    tier: Math.max(0, 4 - point.rank), downgraded: false, basis: 'synthetic_demo', arena: assessment(point) });
  return { source: { source: 'demo-synthetic', fetchedAt: Math.floor(Date.now() / 1000), stale: false, lastError: null },
    sourceUrl: 'https://huggingface.co/datasets/lmarena-ai/leaderboard-dataset', publishedDate: '2026-10-20', category: 'overall',
    entries: structuredClone(demo_points), selectedSelector: selector, selected: selected ? assessment(selected) : null,
    observed: { model: 'demo-current', appType: 'claude_code', observedAt: Math.floor(Date.now() / 1000) },
    candidates: selected ? (selector.startsWith('alias:') ? demo_points.filter((point) => !args.needsImages || point.rank === 1) : [selected]).map(flow) : [{ model: selected_model, provider: 'demo-provider-unrated', tier: null, downgraded: false, basis: 'synthetic_demo', arena: null }],
    fableCandidates: [flow(demo_points[0])], planError: null, configurationGeneration: 0, planSource: 'synthetic_demo', synthetic: true };
}
