import type { Market_catalog, Market_entry } from '../../lib/api/dsh_market.js';
import type { Market_locale } from './market_i18n.js';

export const market_page_size = 12;
export const market_categories = [
  { id: 'personalization', en: 'Personalization', zh: '个性化', icon: 'palette' },
  { id: 'developer_tools', en: 'Developer tools', zh: '开发工具', icon: 'code' },
  { id: 'agents', en: 'Agents & automation', zh: '智能体与自动化', icon: 'workflow' },
  { id: 'models', en: 'Models & providers', zh: '模型与供应商', icon: 'plug' },
  { id: 'data', en: 'Knowledge & data', zh: '知识与数据', icon: 'book' },
  { id: 'integrations', en: 'App integrations', zh: '应用集成', icon: 'message' },
  { id: 'media', en: 'Creative tools', zh: '创意工具', icon: 'palette' },
  { id: 'security', en: 'Security & privacy', zh: '安全与隐私', icon: 'shield' },
  { id: 'observability', en: 'Usage & insights', zh: '用量与观测', icon: 'memory' },
  { id: 'other', en: 'More discoveries', zh: '更多发现', icon: 'grid' },
] as const;
export const market_use_cases = [
  { id: 'customize_ui', en: 'Personalize DSH', zh: '个性化 DSH' },
  { id: 'code_workflow', en: 'Write better code', zh: '辅助编程' },
  { id: 'automate_tasks', en: 'Automate tasks', zh: '自动执行任务' },
  { id: 'connect_models', en: 'Connect models', zh: '接入模型' },
  { id: 'knowledge_research', en: 'Research & knowledge', zh: '研究与知识' },
  { id: 'connect_tools', en: 'Connect tools', zh: '连接工具' },
  { id: 'create_media', en: 'Create content', zh: '创作内容' },
  { id: 'protect_workspace', en: 'Protect my workspace', zh: '保护工作区' },
  { id: 'track_usage', en: 'Track usage', zh: '观察用量' },
  { id: 'desktop_workflow', en: 'Desktop & WebUI', zh: '桌面与 WebUI' },
  { id: 'claude_native', en: 'Claude integration', zh: 'Claude 集成' },
  { id: 'general', en: 'General purpose', zh: '通用场景' },
] as const;
/** Practical module sections, grouped by what part of the harness a plugin touches. */
export const market_modules = [
  { id: 'appearance', en: 'Appearance & UI', zh: '外观与界面', icon: 'palette' },
  { id: 'orchestration', en: 'Orchestration & automation', zh: '编排与自动化', icon: 'workflow' },
  { id: 'reasoning', en: 'Reasoning & CoT', zh: '推理与思维链', icon: 'brain' },
  { id: 'models', en: 'Models & providers', zh: '模型接入', icon: 'plug' },
  { id: 'tools', en: 'Tools & integrations', zh: '工具与集成', icon: 'tools' },
  { id: 'knowledge', en: 'Knowledge & memory', zh: '知识与记忆', icon: 'book' },
  { id: 'media', en: 'Creative media', zh: '创意媒体', icon: 'image' },
  { id: 'developer', en: 'Developer workflow', zh: '开发工作流', icon: 'code' },
  { id: 'general', en: 'General modules', zh: '通用模块', icon: 'grid' },
] as const;

export function category_label(id: string, locale: Market_locale): string {
  return market_categories.find((category) => category.id === id)?.[locale] ?? (locale === 'zh' ? '更多发现' : 'More discoveries');
}
export function use_case_label(id: string, locale: Market_locale): string {
  return market_use_cases.find((use_case) => use_case.id === id)?.[locale] ?? (locale === 'zh' ? '通用场景' : 'General purpose');
}
export function module_label(id: string, locale: Market_locale): string {
  return market_modules.find((module) => module.id === id)?.[locale] ?? (locale === 'zh' ? '通用模块' : 'General modules');
}
export function entry_description(entry: Market_entry, locale: Market_locale): { text: string; original: boolean } {
  const translated = locale === 'zh' ? entry.description_zh : entry.description_en;
  return { text: translated || entry.description, original: !translated };
}
export function github_url(value: string | null | undefined): string | null {
  if (!value) return null;
  try {
    const url = new URL(value);
    if (url.protocol !== 'https:' || url.hostname !== 'github.com' || url.username || url.password || url.port) return null;
    return url.href;
  } catch { return null; }
}
export function registry_source_url(catalog: Market_catalog): string | null {
  return github_url(catalog.registry_url);
}
export function localized_date(value: string | null | undefined, locale: Market_locale): string | null {
  if (!value) return null;
  const timestamp = Date.parse(value);
  if (!Number.isFinite(timestamp)) return null;
  return new Intl.DateTimeFormat(locale === 'zh' ? 'zh-CN' : 'en-US', { dateStyle: 'medium', timeZone: 'UTC' }).format(timestamp);
}
export function format_count(value: number, locale: Market_locale): string {
  return new Intl.NumberFormat(locale === 'zh' ? 'zh-CN' : 'en-US').format(value);
}
export function filter_entries(entries: Market_entry[], query: string, category: string, use_case: string, module: string = 'all'): Market_entry[] {
  const words = query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
  return entries.filter((entry) => {
    if (category !== 'all' && !entry.categories.includes(category)) return false;
    if (use_case !== 'all' && !entry.use_cases.includes(use_case)) return false;
    if (module !== 'all' && !(entry.modules ?? []).includes(module)) return false;
    if (!words.length) return true;
    const text = [entry.name, entry.full_name, entry.description, entry.description_en, entry.description_zh, entry.package_name, entry.author,
      ...entry.categories.map((id) => `${category_label(id, 'en')} ${category_label(id, 'zh')}`),
      ...entry.use_cases.map((id) => `${use_case_label(id, 'en')} ${use_case_label(id, 'zh')}`),
      ...(entry.modules ?? []).map((id) => `${module_label(id, 'en')} ${module_label(id, 'zh')}`)].join(' ').toLocaleLowerCase();
    return words.every((word) => text.includes(word));
  });
}
