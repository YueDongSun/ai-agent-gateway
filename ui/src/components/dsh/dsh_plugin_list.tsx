import { useState } from 'react';
import type { Dsh_operation_request, Dsh_plugin } from '../../lib/api/dsh.js';
import type { Dsh_translate } from './dsh_i18n.js';

interface Dsh_plugin_list_props {
  plugins: Dsh_plugin[];
  locked: boolean;
  initialized: boolean;
  t: Dsh_translate;
  on_preview: (request: Dsh_operation_request) => void;
}

const plugin_filters = ['all', 'personalization', 'practical', 'desktop', 'core'] as const;

function safe_source_url(value: string): string | undefined {
  try {
    const url = new URL(value);
    return url.protocol === 'https:' ? url.href : undefined;
  } catch {
    return undefined;
  }
}

export function Dsh_plugin_list({ plugins, locked, initialized, t, on_preview }: Dsh_plugin_list_props) {
  const [filter, set_filter] = useState<string>('all');
  const [query, set_query] = useState('');
  const [versions, set_versions] = useState<Record<string, string>>({});
  const normalized_query = query.trim().toLowerCase();
  const visible_plugins = plugins.filter((plugin) =>
    (filter === 'all' || plugin.category === filter) &&
    `${plugin.display_name} ${plugin.package_name} ${plugin.description}`.toLowerCase().includes(normalized_query),
  );

  return (
    <section className="dsh_card dsh_plugin_library" aria-labelledby="dsh_plugin_heading">
      <div className="dsh_section_heading">
        <div><h2 id="dsh_plugin_heading">{t('plugins')}</h2><p>{t('plugins_note')}</p></div>
        <span className="dsh_count">{plugins.length}</span>
      </div>
      <div className="dsh_filter_bar">
        <div className="dsh_filters" role="group" aria-label={t('plugins')}>
          {plugin_filters.map((value) => <button
            key={value} type="button" aria-pressed={filter === value} className={filter === value ? 'active' : ''}
            onClick={() => set_filter(value)} data-testid={`dsh_plugin_filter_${value}`}
          >{t(value)}</button>)}
        </div>
        <input type="search" aria-label={t('search_plugins')} placeholder={t('search_plugins')} value={query}
          onChange={(event) => set_query(event.target.value)} data-testid="dsh_plugin_search" />
      </div>
      <div className="dsh_plugin_grid">
        {visible_plugins.map((plugin) => {
          const version = versions[plugin.plugin_id] ?? '';
          const source_url = safe_source_url(plugin.source_url);
          const valid_version = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?(?:\+[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$/.test(version.trim());
          const request = (action: Dsh_operation_request['action']) => on_preview({
            action, package_name: plugin.package_name, version: action === 'install' ? version.trim() : null,
          });
          return <article className="dsh_plugin" key={plugin.plugin_id} data-testid={`dsh_plugin_${plugin.plugin_id}`}>
            <div className="dsh_plugin_heading">
              <span className="dsh_category">{t(plugin.category)}</span>
              {plugin.essential && <span className="dsh_badge">{t('essential')}</span>}
            </div>
            <h3>{plugin.display_name}</h3>
            <code className="dsh_package">{plugin.package_name}</code>
            <p className="dsh_plugin_description">{plugin.description}</p>
            <dl className="dsh_plugin_evidence">
              <div><dt>{t('package')}</dt><dd>{plugin.installed_version ? <span className="dsh_good">{t('installed')} · {plugin.installed_version}</span> : t('absent')}</dd></div>
              <div><dt>{t('configured')}</dt><dd className={plugin.enabled ? 'dsh_good' : ''}>{t(plugin.enabled ? 'enabled' : 'disabled')}</dd></div>
              <div><dt>{t('runtime_phase')}</dt><dd>{t(plugin.runtime_phase)}</dd></div>
            </dl>
            {!plugin.installed_version && <label className="dsh_field">
              <span>{t('version')}</span>
              <input value={version} placeholder={t('version_placeholder')} disabled={locked || !initialized}
                title={t('version_note')} aria-describedby="dsh_version_note"
                onChange={(event) => set_versions((current) => ({ ...current, [plugin.plugin_id]: event.target.value }))}
                data-testid={`dsh_version_${plugin.plugin_id}`} />
            </label>}
            <div className="dsh_actions dsh_plugin_actions">
              {!plugin.installed_version ? <button type="button" className="btn primary" disabled={locked || !initialized || !valid_version}
                onClick={() => request('install')} data-testid={`dsh_install_${plugin.plugin_id}`}>{t('install')}</button> : <>
                <button type="button" className="btn" disabled={locked || (plugin.essential && plugin.enabled)}
                  onClick={() => request(plugin.enabled ? 'disable' : 'enable')}
                  data-testid={`dsh_${plugin.enabled ? 'disable' : 'enable'}_${plugin.plugin_id}`}>{t(plugin.enabled ? 'disable' : 'enable')}</button>
                <button type="button" className="btn dsh_danger_button" disabled={locked || plugin.essential}
                  onClick={() => request('remove')} data-testid={`dsh_remove_${plugin.plugin_id}`}>{t('remove')}</button>
              </>}
              {source_url && <a className="dsh_source" href={source_url} target="_blank" rel="noopener noreferrer">{t('source')} ↗</a>}
            </div>
          </article>;
        })}
      </div>
      {!visible_plugins.length && <p className="dsh_empty">{t('no_plugins')}</p>}
      <p className="dsh_hint" id="dsh_version_note">{t('version_note')}</p>
    </section>
  );
}
