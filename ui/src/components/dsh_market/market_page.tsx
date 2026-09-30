import { Component, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { dsh_market_get_catalog, dsh_market_refresh, type Market_catalog } from '../../lib/api/dsh_market.js';
import { isDemo } from '../../lib/mock/backend.js';
import { category_label, entry_description, filter_entries, format_count, localized_date, market_categories, market_modules, market_page_size, market_use_cases, module_label } from './market_catalog.js';
import { Market_detail } from './market_detail.js';
import { Market_icon } from './market_icons.js';
import { market_translate, type Market_locale } from './market_i18n.js';
import './market_page.css';

type Market_theme = 'light' | 'dark';
type Market_sort = 'stars' | 'name' | 'recent';
function initial_setting(key: 'locale' | 'theme'): string | null {
  const query_value = new URLSearchParams(window.location.search).get(key);
  if (query_value) return query_value;
  try { return window.localStorage.getItem(`aag.market.${key}`) || window.localStorage.getItem(`aag.${key}`); }
  catch { return null; }
}
function initial_locale(): Market_locale {
  const value = initial_setting('locale');
  return value === 'zh' || (!value && navigator.language.startsWith('zh')) ? 'zh' : 'en';
}
function initial_theme(): Market_theme {
  const value = initial_setting('theme');
  return value === 'light' || (value !== 'dark' && window.matchMedia('(prefers-color-scheme: light)').matches) ? 'light' : 'dark';
}
function persist_setting(key: string, value: string) {
  try { window.localStorage.setItem(`aag.market.${key}`, value); } catch { /* The current window still works when storage is disabled. */ }
}

/** No gateway, provider, runtime or diagnostics calls belong in this window. */
export function Dsh_market_page() {
  const [locale, set_locale] = useState<Market_locale>(initial_locale);
  const [theme, set_theme] = useState<Market_theme>(initial_theme);
  const [catalog, set_catalog] = useState<Market_catalog | null>(null);
  const [loading, set_loading] = useState(true);
  const [refreshing, set_refreshing] = useState(false);
  const [error, set_error] = useState<string | null>(null);
  const [notice, set_notice] = useState<'refreshed' | 'refresh_demo' | null>(null);
  const [query, set_query] = useState('');
  const [category, set_category] = useState('all');
  const [use_case, set_use_case] = useState('all');
  const [module, set_module] = useState('all');
  const [sort, set_sort] = useState<Market_sort>('stars');
  const [page, set_page] = useState(1);
  const [selected_id, set_selected_id] = useState<string | null>(null);
  const content_ref = useRef<HTMLDivElement>(null);
  const detail_trigger_ref = useRef<HTMLButtonElement | null>(null);
  const results_ref = useRef<HTMLElement>(null);
  const mounted_ref = useRef(false);
  const request_sequence = useRef(0);
  const refresh_lock = useRef(false);
  const t = market_translate(locale);

  useEffect(() => {
    document.documentElement.lang = locale;
    document.documentElement.setAttribute('data-theme', theme);
    document.title = `${t('title')} · DSH · AAG`;
    persist_setting('locale', locale); persist_setting('theme', theme);
    const current_url = new URL(window.location.href);
    current_url.searchParams.set('locale', locale); current_url.searchParams.set('theme', theme);
    window.history.replaceState(null, '', current_url.href);
  }, [locale, theme]);

  const load_catalog = useCallback(async () => {
    const sequence = ++request_sequence.current;
    set_loading(true); set_error(null);
    try {
      const next = await dsh_market_get_catalog();
      if (mounted_ref.current && sequence === request_sequence.current) set_catalog(next);
    } catch (failure) {
      if (mounted_ref.current && sequence === request_sequence.current) set_error(String(failure));
    } finally {
      if (mounted_ref.current && sequence === request_sequence.current) set_loading(false);
    }
  }, []);
  useEffect(() => {
    mounted_ref.current = true;
    void load_catalog();
    return () => { mounted_ref.current = false; request_sequence.current += 1; };
  }, [load_catalog]);
  useEffect(() => {
    if (selected_id) content_ref.current?.setAttribute('inert', '');
    else content_ref.current?.removeAttribute('inert');
  }, [selected_id]);

  const close_detail = useCallback(() => {
    content_ref.current?.removeAttribute('inert');
    set_selected_id(null);
  }, []);

  const refresh_catalog = async () => {
    if (refresh_lock.current) return;
    refresh_lock.current = true; set_refreshing(true); set_error(null); set_notice(null);
    try {
      const next = await dsh_market_refresh();
      if (mounted_ref.current) { set_catalog(next); set_page(1); close_detail(); set_notice(isDemo ? 'refresh_demo' : 'refreshed'); }
    } catch (failure) { if (mounted_ref.current) set_error(String(failure)); }
    finally { refresh_lock.current = false; if (mounted_ref.current) set_refreshing(false); }
  };

  const category_counts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const entry of catalog?.entries ?? []) for (const id of entry.categories) counts.set(id, (counts.get(id) ?? 0) + 1);
    return counts;
  }, [catalog]);
  const module_counts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const entry of catalog?.entries ?? []) for (const id of entry.modules ?? []) counts.set(id, (counts.get(id) ?? 0) + 1);
    return counts;
  }, [catalog]);
  const filtered = useMemo(() => {
    const entries = filter_entries(catalog?.entries ?? [], query, category, use_case, module);
    entries.sort((left, right) => {
      if (sort === 'stars') return right.stars - left.stars || left.full_name.localeCompare(right.full_name);
      if (sort === 'recent') return (Date.parse(right.updated_at ?? '') || 0) - (Date.parse(left.updated_at ?? '') || 0) || left.full_name.localeCompare(right.full_name);
      return left.name.localeCompare(right.name) || left.full_name.localeCompare(right.full_name);
    });
    return entries;
  }, [catalog, query, category, use_case, module, sort]);
  const page_count = Math.max(1, Math.ceil(filtered.length / market_page_size));
  const current_page = Math.min(page, page_count);
  const offset = (current_page - 1) * market_page_size;
  const visible_entries = filtered.slice(offset, offset + market_page_size);
  const selected_entry = catalog?.entries.find((entry) => entry.id === selected_id);
  const has_filters = Boolean(query || category !== 'all' || use_case !== 'all' || module !== 'all');
  const clear_filters = () => { set_query(''); set_category('all'); set_use_case('all'); set_module('all'); set_page(1); };
  const set_result_page = (next: number) => { set_page(next); results_ref.current?.scrollIntoView({ behavior: 'auto', block: 'start' }); };

  return <div className="market_page" data-testid="market_page" data-locale={locale} data-theme={theme}>
    <div ref={content_ref}>
      <header className="market_header"><div className="market_header_inner">
        <div className="market_brand"><span className="market_brand_icon"><Market_icon name="grid" /></span><div><strong>{t('brand')}</strong><span>{t('workspace')}</span></div></div>
        <div className="market_header_controls"><span className="market_preview_tag">{isDemo ? t('preview') : 'AAG'}</span>
          <div className="market_language" role="group" aria-label={t('language')}>
            <button type="button" aria-pressed={locale === 'en'} onClick={() => set_locale('en')} data-testid="market_locale_en" lang="en">EN</button>
            <button type="button" aria-pressed={locale === 'zh'} onClick={() => set_locale('zh')} data-testid="market_locale_zh" lang="zh">中文</button>
          </div>
          <button type="button" className="market_icon_button" aria-label={t(theme === 'dark' ? 'theme_light' : 'theme_dark')} title={t(theme === 'dark' ? 'theme_light' : 'theme_dark')} data-testid="market_theme"
            onClick={() => set_theme(theme === 'dark' ? 'light' : 'dark')}><Market_icon name={theme === 'dark' ? 'sun' : 'moon'} /></button>
        </div>
      </div></header>
      <main className="market_main">
        <section className="market_hero" aria-labelledby="market_title"><div className="market_hero_content"><span className="market_eyebrow"><span className="market_spark" />{t('eyebrow')}</span>
          <h1 id="market_title" data-testid="market_title">{t('hero_title')}</h1><p>{t('hero_description')}</p>
          <label className="market_search"><Market_icon name="search" /><span className="market_sr_only">{t('search_label')}</span><input type="search" value={query} onChange={(event) => { set_query(event.target.value); set_page(1); }} placeholder={t('search')} data-testid="market_search" />{query && <button type="button" className="market_icon_button" aria-label={t('clear')} onClick={() => { set_query(''); set_page(1); }}><Market_icon name="close" /></button>}</label>
          <div className="market_metrics"><div><strong>{catalog ? format_count(catalog.entries.length, locale) : '—'}</strong><span>{t('catalog_entries')}</span></div><div><strong>{catalog ? category_counts.size : '—'}</strong><span>{t('functional_categories')}</span></div><div><strong>{catalog ? '1' : '—'}</strong><span>{t('source_registries')}</span></div></div>
        </div><div className="market_hero_art" aria-hidden="true"><div className="market_art_orbit market_art_orbit_outer" /><div className="market_art_orbit" /><span className="market_art_center"><Market_icon name="grid" /></span><span className="market_art_tile market_art_code"><Market_icon name="code" /></span><span className="market_art_tile market_art_palette"><Market_icon name="palette" /></span><span className="market_art_tile market_art_plug"><Market_icon name="plug" /></span><span className="market_art_tile market_art_workflow"><Market_icon name="workflow" /></span></div></section>
        <div className="market_safety_strip"><Market_icon name="shield" /><p><strong>{t('readonly')}</strong> {t('readonly_note')}</p></div>
        {error && <div className="market_error_box" role="alert" data-testid="market_error"><strong>{catalog ? t('refresh_failed') : t('catalog_failed')}</strong><span>{error}</span>{!catalog && <button type="button" className="market_button" onClick={() => void load_catalog()} disabled={loading}>{t('retry')}</button>}</div>}
        {notice && <p className="market_status" role="status" data-testid="market_notice">{t(notice)}</p>}
        {catalog?.warning && <p className="market_warning" role="status">{catalog.warning}</p>}
        <div className="market_workspace"><aside className="market_sidebar" aria-label={t('categories')}><h2>{t('categories')}</h2>
          <nav className="market_categories"><button type="button" aria-pressed={category === 'all'} onClick={() => { set_category('all'); set_page(1); }} data-testid="market_category_all"><Market_icon name="grid" /><span>{t('all_categories')}</span><small>{catalog ? format_count(catalog.entries.length, locale) : '—'}</small></button>
            {market_categories.map((item) => <button type="button" key={item.id} aria-pressed={category === item.id} onClick={() => { set_category(item.id); set_page(1); }} data-testid={`market_category_${item.id}`}><Market_icon name={item.icon} /><span>{item[locale]}</span><small>{format_count(category_counts.get(item.id) ?? 0, locale)}</small></button>)}
          </nav><h2 className="market_sidebar_subhead">{t('modules')}</h2>
          <nav className="market_categories" aria-label={t('modules')}><button type="button" aria-pressed={module === 'all'} onClick={() => { set_module('all'); set_page(1); }} data-testid="market_module_all"><Market_icon name="grid" /><span>{t('all_modules')}</span><small>{catalog ? format_count(catalog.entries.length, locale) : '—'}</small></button>
            {market_modules.map((item) => <button type="button" key={item.id} aria-pressed={module === item.id} onClick={() => { set_module(item.id); set_page(1); }} data-testid={`market_module_${item.id}`}><Market_icon name={item.icon} /><span>{item[locale]}</span><small>{format_count(module_counts.get(item.id) ?? 0, locale)}</small></button>)}
          </nav><div className="market_sidebar_note"><Market_icon name="book" /><strong>{t('source_health')}</strong><p>{catalog?.source_registry ?? 'GitHub'}</p>{catalog && <><code title={catalog.registry_commit}>{catalog.registry_commit.slice(0, 12)}</code><span>{t(catalog.cache_status === 'bundled' ? 'bundled' : catalog.cache_status === 'cached' ? 'cached' : 'live')}</span></>}<p>{t('taxonomy_note')}</p></div>
        </aside><section className="market_results" aria-labelledby="market_results_title" ref={results_ref}>
          <div className="market_workflow_section"><h2>{t('use_cases')}</h2><div className="market_workflow_filters" role="group" aria-label={t('use_cases')}><button type="button" aria-pressed={use_case === 'all'} data-testid="market_use_case_all" onClick={() => { set_use_case('all'); set_page(1); }}>{t('all_use_cases')}</button>{market_use_cases.map((item) => <button type="button" key={item.id} aria-pressed={use_case === item.id} data-testid={`market_use_case_${item.id}`} onClick={() => { set_use_case(item.id); set_page(1); }}>{item[locale]}</button>)}</div></div>
          <div className="market_results_toolbar"><div><h2 id="market_results_title">{category === 'all' ? t('browse') : category_label(category, locale)}</h2><p aria-live="polite" data-testid="market_results_count" data-count={filtered.length}>{t('results', { count: format_count(filtered.length, locale) })}</p></div><div className="market_sort_controls">{has_filters && <button type="button" className="market_text_button" onClick={clear_filters} data-testid="market_clear_filters">{t('clear')}</button>}<label><span className="market_sr_only">{t('sort')}</span><select value={sort} onChange={(event) => { set_sort(event.target.value as Market_sort); set_page(1); }} data-testid="market_sort"><option value="stars">{t('sort_stars')}</option><option value="recent">{t('sort_recent')}</option><option value="name">{t('sort_name')}</option></select></label></div></div>
          {loading ? <div className="market_loading" role="status"><span className="market_loading_dot" />{t('loading')}</div> : <>
            {!visible_entries.length && <div className="market_empty" data-testid="market_empty"><Market_icon name="search" /><h3>{t('no_results')}</h3><p>{t('no_results_hint')}</p>{has_filters && <button type="button" className="market_button" onClick={clear_filters}>{t('clear')}</button>}</div>}
            <div className="market_grid">{visible_entries.map((entry) => {
              const description = entry_description(entry, locale);
              const primary_category = market_categories.find((item) => item.id === entry.categories[0]);
              return <article className="market_card" key={entry.id} data-testid="market_card" data-entry-id={entry.id} data-categories={entry.categories.join(',')} data-use-cases={entry.use_cases.join(',')}>
                <div className="market_card_top"><span className={`market_plugin_icon market_tint_${entry.categories[0] ?? 'other'}`}><Market_icon name={primary_category?.icon ?? 'plug'} /></span><span className="market_stars" title={t('stars')}><Market_icon name="star" />{format_count(entry.stars, locale)}</span></div>
                <h3 title={entry.name}>{entry.name}</h3><p className="market_card_author" title={entry.author}>{entry.author}</p><p className="market_card_description">{description.text || t('no_description')}</p>
                {description.original && description.text && <span className="market_original">{t('original_description')}</span>}
                <div className="market_card_tags"><span>{category_label(entry.categories[0] ?? 'other', locale)}</span>{entry.has_client && <span><Market_icon name="monitor" />{locale === 'zh' ? '含客户端' : 'Client included'}</span>}</div>
                <div className="market_card_footer"><span className="market_indexed" title={t('not_verified')}><Market_icon name="code" />{t('source_listed')}</span><button type="button" aria-label={`${t('details')}: ${entry.name}`} onClick={(event) => { detail_trigger_ref.current = event.currentTarget; set_selected_id(entry.id); }} data-testid={`market_detail_${entry.id}`}>{t('details')}<Market_icon name="arrow" /></button></div>
              </article>;
            })}</div>
            {filtered.length > 0 && <nav className="market_pagination" aria-label={t('pagination')}><span>{t('showing', { start: format_count(offset + 1, locale), end: format_count(Math.min(offset + market_page_size, filtered.length), locale), count: format_count(filtered.length, locale) })}</span><div><button type="button" className="market_button" disabled={current_page <= 1} onClick={() => set_result_page(current_page - 1)} data-testid="market_previous">{t('previous')}</button><span>{t('page', { page: current_page, pages: page_count })}</span><button type="button" className="market_button" disabled={current_page >= page_count} onClick={() => set_result_page(current_page + 1)} data-testid="market_next">{t('next')}</button></div></nav>}
          </>}
        </section></div>
        <footer className="market_footer"><div><strong>{t('collection')}</strong><p>{catalog ? `${catalog.source_registry} · ${t('snapshot_date')} ${localized_date(catalog.fetched_at, locale) ?? t('unknown')} UTC` : t('loading')}</p><span>{isDemo ? t('preview_note') : t('evidence_note')}</span></div><button type="button" className="market_button" disabled={loading || refreshing} onClick={() => void refresh_catalog()} data-testid="market_refresh"><Market_icon name="refresh" class_name={refreshing ? 'market_spinning' : ''} />{t(refreshing ? 'refreshing' : 'refresh')}</button></footer>
      </main>
    </div>
    {selected_entry && catalog && <Market_detail key={selected_entry.id} entry={selected_entry} catalog={catalog} locale={locale} t={t} return_focus={detail_trigger_ref.current} on_close={close_detail} />}
  </div>;
}

/** Rendering failures in this restricted window must not invoke diagnostics IPC. */
export class Dsh_market_boundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  state = { failed: false };
  static getDerivedStateFromError() { return { failed: true }; }
  render() {
    if (!this.state.failed) return this.props.children;
    const t = market_translate(initial_locale());
    return <div className="market_fatal" role="alert"><h1>{t('error_title')}</h1><button type="button" onClick={() => window.location.reload()}>{t('reload')}</button></div>;
  }
}
