import type { Locale } from '../../../i18n/index.js';

const provider_ui_copy = {
  en: {
    search: 'Search providers',
    search_hint: 'Name or endpoint',
    clear: 'Clear search',
    no_results: 'No providers match your search.',
    reorder_hint: 'Clear the search to change provider order.',
    more: 'More',
    less: 'Less',
    enabled: 'Enabled',
    pinned: 'Pin',
    auto_track: 'Track models',
    results: (visible: number, total: number) => `${visible} of ${total} providers`,
  },
  zh: {
    search: '搜索供应商',
    search_hint: '名称或端点',
    clear: '清除搜索',
    no_results: '没有匹配的供应商。',
    reorder_hint: '清除搜索后可调整供应商顺序。',
    more: '更多',
    less: '收起',
    enabled: '启用',
    pinned: '固定',
    auto_track: '跟踪模型',
    results: (visible: number, total: number) => `${visible} / ${total} 个供应商`,
  },
} as const;

export function get_provider_ui_copy(locale: Locale) {
  return provider_ui_copy[locale];
}
