import { useEffect, useRef, useState } from 'react';
import { dsh_market_open_source, type Market_catalog, type Market_entry, type Market_source_kind } from '../../lib/api/dsh_market.js';
import { isDemo } from '../../lib/mock/backend.js';
import { category_label, entry_description, format_count, github_url, localized_date, module_label, registry_source_url, use_case_label } from './market_catalog.js';
import type { Market_locale, Market_translate } from './market_i18n.js';
import { Market_icon } from './market_icons.js';

export function Market_source_link({ entry_id, kind, url, label, t, on_error, class_name = '' }: {
  entry_id: string; kind: Market_source_kind; url: string | null; label: string; t: Market_translate;
  on_error: (message: string) => void; class_name?: string;
}) {
  const safe_url = kind === 'registry' && url === 'https://dsh.market/plugins.json' ? url : github_url(url);
  if (!safe_url) return null;
  const content = <><span>{label}</span><Market_icon name="external" /></>;
  return isDemo ? <a className={`market_source_link ${class_name}`} data-testid={`market_source_${kind}`} href={safe_url} target="_blank" rel="noopener noreferrer" title={t('open_external')}>{content}</a>
    : <button type="button" className={`market_source_link ${class_name}`} data-testid={`market_source_${kind}`} title={t('open_external')} onClick={() => {
      void dsh_market_open_source(entry_id, kind).catch((error) => on_error(String(error)));
    }}>{content}</button>;
}

export function Market_detail({ entry, catalog, locale, t, return_focus, on_close }: {
  entry: Market_entry; catalog: Market_catalog; locale: Market_locale; t: Market_translate; return_focus: HTMLElement | null; on_close: () => void;
}) {
  const dialog_ref = useRef<HTMLElement>(null);
  const close_ref = useRef<HTMLButtonElement>(null);
  const [message, set_message] = useState<string | null>(null);
  const [error, set_error] = useState<string | null>(null);
  const description = entry_description(entry, locale);
  useEffect(() => {
    // The launcher is captured by the click handler, not by a replayed effect.
    const previous_focus = return_focus ?? document.activeElement;
    const previous_overflow = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    close_ref.current?.focus();
    return () => {
      document.body.style.overflow = previous_overflow;
      if (previous_focus instanceof HTMLElement && previous_focus.isConnected) previous_focus.focus();
    };
  }, []);
  const copy = async (value: string) => {
    try { await navigator.clipboard.writeText(value); set_error(null); set_message(t('copied')); }
    catch { set_message(null); set_error(t('copy_failed')); }
  };
  return <div className="market_modal_backdrop" onClick={on_close}>
    <section className="market_detail" role="dialog" aria-modal="true" aria-labelledby="market_detail_title" data-testid="market_detail_dialog" ref={dialog_ref} tabIndex={-1}
      onClick={(event) => event.stopPropagation()} onKeyDown={(event) => {
        if (event.key === 'Escape') { event.stopPropagation(); on_close(); }
        if (event.key !== 'Tab') return;
        const focusable = dialog_ref.current?.querySelectorAll<HTMLElement>('button:not(:disabled), a[href], input:not(:disabled), select:not(:disabled), [tabindex="0"]');
        if (!focusable?.length) return;
        const first = focusable[0]; const last = focusable[focusable.length - 1];
        if (event.shiftKey && (document.activeElement === first || document.activeElement === dialog_ref.current)) { event.preventDefault(); last.focus(); }
        else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
      }}>
      <div className="market_detail_top"><span className="market_eyebrow">{t('details')}</span><button ref={close_ref} type="button" className="market_icon_button" aria-label={t('close')} onClick={on_close} data-testid="market_detail_close"><Market_icon name="close" /></button></div>
      <div className={`market_plugin_icon market_tint_${entry.categories[0] ?? 'other'}`}><Market_icon name="plug" /></div>
      <h2 id="market_detail_title">{entry.name}</h2>
      <p className="market_repo_name">{entry.full_name}</p>
      <div className="market_detail_badges"><span className="market_badge"><Market_icon name="star" />{format_count(entry.stars, locale)}</span><span className="market_badge">{t('not_verified')}</span></div>
      <p className="market_detail_description">{description.text || t('no_description')}</p>
      {description.original && description.text && <p className="market_original" data-testid="market_original_description">{t('original_description')}</p>}
      <div className="market_detail_tags">{entry.categories.map((id) => <span className="market_tag" key={id}>{category_label(id, locale)}</span>)}{entry.use_cases.map((id) => <span className="market_tag market_tag_subtle" key={id}>{use_case_label(id, locale)}</span>)}{(entry.modules ?? []).map((id) => <span className="market_tag market_tag_module" key={id}>{module_label(id, locale)}</span>)}</div>
      <div className="market_detail_actions"><Market_source_link entry_id={entry.id} kind="repository" url={entry.repo_url} label={t('source')} t={t} on_error={set_error} class_name="market_primary" />
        <button type="button" className="market_button" onClick={() => void copy(entry.repo_url)}><Market_icon name="copy" />{t('copy_source')}</button></div>
      {error && <p className="market_error" role="alert">{error}</p>}{message && <p className="market_status" role="status">{message}</p>}
      <section className="market_detail_section"><h3><Market_icon name="code" />{t('package_metadata')}</h3>
        <dl className="market_metadata">
          <div><dt>{t('package_name')}</dt><dd>{entry.package_name ? <code>{entry.package_name}</code> : t('unavailable')}</dd></div>
          <div><dt>{t('version')}</dt><dd>{entry.version || t('unknown')}</dd></div>
          <div><dt>{t('install_method')}</dt><dd>{entry.install_method === 'unknown' ? t('unknown') : entry.install_method}</dd></div>
          <div><dt>{t('license')}</dt><dd>{entry.license || t('unknown')}</dd></div>
          <div><dt>{t('platforms')}</dt><dd>{t('unknown')}</dd></div>
          <div><dt>{t('compatibility')}</dt><dd>{t('not_verified')}</dd></div>
        </dl>
        {entry.package_name && <button type="button" className="market_button" onClick={() => void copy(entry.package_name!)} data-testid="market_copy_package"><Market_icon name="copy" />{t('copy_package')}</button>}
        {entry.install_spec && <div className="market_install_reference"><h4>{t('install_reference')}</h4><code>{entry.install_spec}</code><p>{t('install_reference_note')}</p></div>}
      </section>
      <section className="market_detail_section"><h3><Market_icon name="book" />{t('provenance')}</h3>
        <p className="market_small_note">{t('source_note')}</p>
        <dl className="market_metadata">
          <div><dt>{t('owner')}</dt><dd>{entry.author}</dd></div>
          <div><dt>{t('source_kind')}</dt><dd>{catalog.source_registry}</dd></div>
          <div><dt>{t('registry_commit')}</dt><dd><code data-testid="market_registry_commit">{catalog.registry_commit || t('commit_unknown')}</code></dd></div>
          <div><dt>{t('plugin_commit')}</dt><dd><code data-testid="market_plugin_commit">{entry.source_commit || t('commit_unknown')}</code></dd></div>
          <div><dt>{t('registry_updated')}</dt><dd>{localized_date(catalog.registry_generated_at, locale) || t('unknown')} UTC</dd></div>
          <div><dt>{t('snapshot_date')}</dt><dd>{localized_date(catalog.fetched_at, locale) || t('unknown')} UTC</dd></div>
        </dl>
        <div className="market_provenance_links"><Market_source_link entry_id={entry.id} kind="registry" url={registry_source_url(catalog)} label={t('exact_source')} t={t} on_error={set_error} />
          <Market_source_link entry_id={entry.id} kind="collector" url={catalog.collector_url} label={locale === 'zh' ? '自动采集脚本源码' : 'Collector script source'} t={t} on_error={set_error} />
          {entry.source_commit_url && <Market_source_link entry_id={entry.id} kind="commit" url={entry.source_commit_url} label={t('plugin_commit')} t={t} on_error={set_error} />}</div>
      </section>
      <div className="market_trust_note"><Market_icon name="shield" /><p>{t('detail_warning')}</p></div>
      <p className="market_small_note">{t('evidence_note')}</p>
    </section>
  </div>;
}
