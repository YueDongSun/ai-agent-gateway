/** One-click provider templates ("provider customed config"): well-known
 *  upstream vendors with an Anthropic-protocol endpoint. Picking a
 *  template prefills the unified provider modal; the normal save path then
 *  creates the preset card, the linked `ccs-*` gateway upstream (failover
 *  chain member by default), and — via the identity `modelMapping` — one
 *  route per template model at the next proxy start. Model lists are
 *  minimal seeds: model auto-discovery keeps them current afterwards.
 *
 *  Catalog breadth ported from Magpie's `internal/provider/presets.go`
 *  (gap-analysis item 7): only the presets that publish an Anthropic-
 *  protocol endpoint are listed, because the unified modal seeds an
 *  `anthropic` upstream. `category` mirrors Magpie's preset Kind so the
 *  picker groups vendors, aggregators, and local servers apart. */

export type PresetCategory = 'vendor' | 'relay' | 'local';

export interface ProviderPresetTemplate {
  /** Stable snake_case identifier, becomes the card id. */
  id: string;
  name: string;
  /** Anthropic-protocol base URL for the vendor's endpoint. */
  baseURL: string;
  /** Seed upstream models; never removed by discovery. */
  models: string[];
  websiteUrl: string;
  /** Designed glyph id from `provider_icon.tsx` (snake_case, one per
   *  brand family); any other non-empty value is rendered as a literal
   *  character on the tile. */
  icon: string;
  /** Brand tile background for the icon. */
  iconColor: string;
  /** Picker grouping (Magpie's PresetDef.Kind): the model's own maker, an
   *  aggregator reselling many vendors, or something on this machine. */
  category: PresetCategory;
  /** One line under the name in the picker: plan scope, region, caveat. */
  note?: string;
}

export const PRESET_CATEGORIES: PresetCategory[] = ['vendor', 'relay', 'local'];

export const PROVIDER_PRESET_TEMPLATES: ProviderPresetTemplate[] = [
  // ---- vendor: the model's own maker ----
  {
    id: 'anthropic',
    name: 'Anthropic',
    baseURL: 'https://api.anthropic.com',
    models: ['claude-fable-5', 'claude-opus-4-8', 'claude-sonnet-5'],
    websiteUrl: 'https://console.anthropic.com',
    icon: 'spark',
    iconColor: '#d97757',
    category: 'vendor',
    note: 'Official API · pay as you go',
  },
  {
    id: 'glm_coding_plan',
    name: 'GLM Coding Plan',
    baseURL: 'https://open.bigmodel.cn/api/anthropic',
    models: ['glm-5.3', 'glm-5.3-flash', 'glm-4.7'],
    websiteUrl: 'https://coding.bigmodel.cn',
    icon: 'nodes',
    iconColor: '#3b82f6',
    category: 'vendor',
    note: 'Zhipu · Coding Plan',
  },
  {
    id: 'zai_glm',
    name: 'Z.ai GLM',
    baseURL: 'https://api.z.ai/api/anthropic',
    models: ['glm-5.3', 'glm-5.2'],
    websiteUrl: 'https://z.ai',
    icon: 'nodes',
    iconColor: '#2563eb',
    category: 'vendor',
    note: 'Pay as you go',
  },
  {
    id: 'kimi_coding_plan',
    name: 'Kimi For Coding',
    baseURL: 'https://api.moonshot.cn/anthropic',
    models: ['kimi-k3', 'kimi-k2.6'],
    websiteUrl: 'https://platform.moonshot.cn',
    icon: 'moon',
    iconColor: '#111827',
    category: 'vendor',
    note: 'Moonshot · China',
  },
  {
    id: 'kimi_code',
    name: 'Kimi Code (Global)',
    baseURL: 'https://api.kimi.ai/coding',
    models: ['kimi-k3', 'kimi-k2.6'],
    websiteUrl: 'https://www.kimi.com/code',
    icon: 'moon',
    iconColor: '#1f2937',
    category: 'vendor',
    note: 'kimi.com/code · Global',
  },
  {
    id: 'kimi_code_cn',
    name: 'Kimi Code (China)',
    baseURL: 'https://api.kimi.com/coding',
    models: ['kimi-k3', 'kimi-k2.6'],
    websiteUrl: 'https://www.kimi.com/code',
    icon: 'moon',
    iconColor: '#374151',
    category: 'vendor',
    note: 'kimi.com/code · China',
  },
  {
    id: 'minimax_coding_plan',
    name: 'MiniMax Coding Plan',
    baseURL: 'https://api.minimaxi.com/anthropic',
    models: ['MiniMax-M3'],
    websiteUrl: 'https://www.minimaxi.com',
    icon: 'waves',
    iconColor: '#ef4444',
    category: 'vendor',
    note: 'China',
  },
  {
    id: 'minimax_global',
    name: 'MiniMax (Global)',
    baseURL: 'https://api.minimax.io/anthropic',
    models: ['MiniMax-M3'],
    websiteUrl: 'https://platform.minimax.io',
    icon: 'waves',
    iconColor: '#dc2626',
    category: 'vendor',
  },
  {
    id: 'deepseek',
    name: 'DeepSeek',
    baseURL: 'https://api.deepseek.com/anthropic',
    models: ['deepseek-chat', 'deepseek-reasoner'],
    websiteUrl: 'https://platform.deepseek.com',
    icon: 'whale',
    iconColor: '#4f46e5',
    category: 'vendor',
    note: 'Pay as you go',
  },
  {
    id: 'xai_grok',
    name: 'xAI Grok',
    baseURL: 'https://api.x.ai',
    models: [],
    websiteUrl: 'https://console.x.ai',
    icon: 'cross',
    iconColor: '#111827',
    category: 'vendor',
  },
  {
    id: 'stepfun_step_plan',
    name: 'StepFun Step Plan',
    baseURL: 'https://api.stepfun.ai/step_plan',
    models: [],
    websiteUrl: 'https://platform.stepfun.ai',
    icon: 'steps',
    iconColor: '#0ea5e9',
    category: 'vendor',
    note: 'Global',
  },
  {
    id: 'xiaomi_mimo',
    name: 'Xiaomi MiMo',
    baseURL: 'https://token-plan-cn.xiaomimimo.com/anthropic',
    models: ['mimo-v2.5-pro'],
    websiteUrl: 'https://www.xiaomimimo.com',
    icon: 'chip',
    iconColor: '#f97316',
    category: 'vendor',
    note: 'Token Plan · China',
  },
  {
    id: 'baidu_qianfan',
    name: 'Baidu Qianfan',
    baseURL: 'https://qianfan.baidubce.com/anthropic/tokenplan/personal',
    models: ['qianfan-code-latest', 'deepseek-v4.1-flash', 'deepseek-v4-pro'],
    websiteUrl: 'https://cloud.baidu.com',
    icon: 'sail',
    iconColor: '#2563eb',
    category: 'vendor',
    note: 'Token Plan Personal',
  },
  {
    id: 'tencent_cloud',
    name: 'Tencent Cloud',
    baseURL: 'https://api.lkeap.cloud.tencent.com/plan/anthropic',
    models: ['tc-code-latest', 'glm-5.3', 'kimi-k3'],
    websiteUrl: 'https://cloud.tencent.com',
    icon: 'cloud',
    iconColor: '#0d9488',
    category: 'vendor',
    note: 'Token Plan · China',
  },
  {
    id: 'huaweicloud_maas',
    name: 'Huawei Cloud MaaS',
    baseURL: 'https://api.modelarts-maas.com/plan/anthropic',
    models: ['glm-5.3', 'kimi-k2.6', 'deepseek-v4.1-flash'],
    websiteUrl: 'https://huaweicloud.com',
    icon: 'petals',
    iconColor: '#c2410c',
    category: 'vendor',
    note: 'Token Plan',
  },
  {
    id: 'volcengine_ark',
    name: 'Volcengine Ark',
    baseURL: 'https://ark.cn-beijing.volces.com/api/coding',
    models: ['ark-code-latest', 'doubao-seed-2.1-pro', 'doubao-seed-2.1-lite'],
    websiteUrl: 'https://www.volcengine.com',
    icon: 'volcano',
    iconColor: '#ea580c',
    category: 'vendor',
    note: 'Coding Plan · Beijing',
  },
  {
    id: 'qwen_dashscope',
    name: 'Qwen (Global)',
    baseURL: 'https://dashscope-intl.aliyuncs.com/apps/anthropic',
    models: ['qwen3.8-max', 'qwen3.7-max', 'qwen3.7-plus'],
    websiteUrl: 'https://modelstudio.console.alibabacloud.com',
    icon: 'gem',
    iconColor: '#7c3aed',
    category: 'vendor',
    note: 'Alibaba Model Studio · Global',
  },
  {
    id: 'qwen_dashscope_cn',
    name: 'Qwen (China)',
    baseURL: 'https://dashscope.aliyuncs.com/apps/anthropic',
    models: ['qwen3.8-max', 'qwen3.7-max', 'qwen3.7-plus'],
    websiteUrl: 'https://bailian.console.aliyun.com',
    icon: 'gem',
    iconColor: '#6d28d9',
    category: 'vendor',
    note: 'Bailian · China',
  },
  {
    id: 'ollama_cloud',
    name: 'Ollama Cloud',
    baseURL: 'https://ollama.com',
    models: [],
    websiteUrl: 'https://docs.ollama.com/cloud',
    icon: 'llama',
    iconColor: '#65a30d',
    category: 'vendor',
    note: 'Hosted Ollama',
  },
  // ---- relay: aggregators reselling many vendors ----
  {
    id: 'openrouter',
    name: 'OpenRouter',
    baseURL: 'https://openrouter.ai/api',
    models: [],
    websiteUrl: 'https://openrouter.ai',
    icon: 'branch',
    iconColor: '#6366f1',
    category: 'relay',
    note: 'Many vendors behind one key',
  },
  {
    id: 'aihubmix',
    name: 'AiHubMix',
    baseURL: 'https://aihubmix.com',
    models: [],
    websiteUrl: 'https://aihubmix.com',
    icon: 'flask',
    iconColor: '#0891b2',
    category: 'relay',
  },
  {
    id: 'pipellm',
    name: 'PipeLLM',
    baseURL: 'https://api.pipellm.ai',
    models: [],
    websiteUrl: 'https://www.pipellm.ai',
    icon: 'pipe',
    iconColor: '#475569',
    category: 'relay',
  },
  {
    id: '302ai',
    name: '302.AI',
    baseURL: 'https://api.302.ai',
    models: [],
    websiteUrl: 'https://302.ai',
    icon: 'robot',
    iconColor: '#0f766e',
    category: 'relay',
  },
  {
    id: 'cherryin',
    name: 'CherryIN',
    baseURL: 'https://open.cherryin.ai',
    models: [],
    websiteUrl: 'https://open.cherryin.ai',
    icon: 'cherry',
    iconColor: '#be123c',
    category: 'relay',
  },
  {
    id: 'opencode_zen',
    name: 'OpenCode Zen',
    baseURL: 'https://opencode.ai/zen',
    models: [],
    websiteUrl: 'https://opencode.ai/docs/zen',
    icon: 'lotus',
    iconColor: '#334155',
    category: 'relay',
    note: 'No key required',
  },
  {
    id: 'opencode_go',
    name: 'OpenCode Go',
    baseURL: 'https://opencode.ai/zen/go',
    models: [],
    websiteUrl: 'https://opencode.ai/docs/go',
    icon: 'motion',
    iconColor: '#1e293b',
    category: 'relay',
  },
  {
    id: 'commandcode',
    name: 'Command Code',
    baseURL: 'https://api.commandcode.ai/provider',
    models: [],
    websiteUrl: 'https://commandcode.ai/docs/provider',
    icon: 'keyboard',
    iconColor: '#4338ca',
    category: 'relay',
  },
  {
    id: 'yylx',
    name: '鱼鱼连线',
    baseURL: 'https://app.yylx.io',
    models: [],
    websiteUrl: 'https://yylx.io',
    icon: 'fish',
    iconColor: '#0284c7',
    category: 'relay',
  },
  // ---- local: something running on this machine ----
  {
    id: 'ollama',
    name: 'Ollama',
    baseURL: 'http://localhost:11434',
    models: [],
    websiteUrl: 'https://ollama.com',
    icon: 'llama',
    iconColor: '#65a30d',
    category: 'local',
    note: 'Your local models · port 11434',
  },
  {
    id: 'omlx',
    name: 'oMLX',
    baseURL: 'http://localhost:8000',
    models: [],
    websiteUrl: 'https://omlx.ai',
    icon: 'apple',
    iconColor: '#a1a1aa',
    category: 'local',
    note: 'Local server · port 8000',
  },
  {
    id: 'mlx_serve',
    name: 'MLX-Serve',
    baseURL: 'http://localhost:11234',
    models: [],
    websiteUrl: 'https://github.com/ddalcu/mlx-serve',
    icon: 'gear',
    iconColor: '#71717a',
    category: 'local',
    note: 'Local server · port 11234',
  },
];

/** Identity display→upstream mapping for the seed models: each entry makes
 *  the proxy auto-create a route (alias = model id → this provider's hop)
 *  at proxy start, and gives model discovery an upgrade anchor. */
export function identity_model_mapping(
  models: string[],
): Record<string, string> {
  return Object.fromEntries(models.map((m) => [m, m]));
}

/** Presets filtered by a free-text query (name, note, or base URL), kept
 *  in catalog order within their category. Empty query returns all. */
export function filter_presets(
  presets: ProviderPresetTemplate[],
  query: string,
): ProviderPresetTemplate[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return presets;
  return presets.filter((preset) =>
    [preset.name, preset.note ?? '', preset.baseURL, preset.id]
      .join(' ')
      .toLowerCase()
      .includes(needle),
  );
}
