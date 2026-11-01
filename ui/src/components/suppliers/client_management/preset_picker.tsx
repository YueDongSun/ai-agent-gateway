import { useId, useMemo, useState } from 'react';
import { useT } from '../../../lib/ctx';
import {
  PRESET_CATEGORIES,
  PROVIDER_PRESET_TEMPLATES,
  filter_presets,
  type PresetCategory,
  type ProviderPresetTemplate,
} from '../../../lib/provider_presets.js';
import { Provider_icon } from './provider_icon.js';
import { use_accessible_dialog } from '../../use_accessible_dialog.js';

interface PresetPickerProps {
  /** Hand the chosen template id back to the panel (which owns the modal
   *  prefill flow). The picker closes itself. */
  onPick: (templateId: string) => void;
  onClose: () => void;
}

/** Searchable, category-grouped provider preset picker.
 *
 *  The old surface was a bare `<select>`; with the preset catalog now at
 *  vendor breadth (Magpie parity, gap-analysis item 7) a flat dropdown
 *  stopped being scannable. This mirrors Magpie's add-sheet grouping:
 *  vendors first, then relays/aggregators, then local servers, with a
 *  free-text filter over name, note, and base URL. */
export function Preset_picker({ onPick, onClose }: PresetPickerProps) {
  const t = useT();
  const title_id = useId();
  const dialog_ref = use_accessible_dialog(onClose);
  const [query, setQuery] = useState('');
  const filtered = useMemo(
    () => filter_presets(PROVIDER_PRESET_TEMPLATES, query),
    [query],
  );
  const grouped = useMemo(
    () =>
      PRESET_CATEGORIES.map((category) => ({
        category,
        entries: filtered.filter(
          (preset: ProviderPresetTemplate) => preset.category === category,
        ),
      })).filter((group) => group.entries.length > 0),
    [filtered],
  );

  return (
    <div className="modal-backdrop" onClick={onClose} data-testid="preset-picker-backdrop">
      <div
        className="modal preset-picker"
        ref={dialog_ref}
        role="dialog"
        aria-modal="true"
        aria-labelledby={title_id}
        tabIndex={-1}
        onClick={(e) => e.stopPropagation()}
        data-testid="preset-picker"
      >
        <h3 id={title_id}>{t('providers.presetPicker.title')}</h3>
        <input
          className="input preset-picker-search"
          type="search"
          value={query}
          placeholder={t('providers.presetPicker.search')}
          aria-label={t('providers.presetPicker.search')}
          data-testid="preset-picker-search"
          onChange={(e) => setQuery(e.target.value)}
        />
        <div className="preset-picker-list" data-testid="preset-picker-list">
          {grouped.length === 0 && (
            <p className="muted" data-testid="preset-picker-empty">
              {t('providers.presetPicker.empty')}
            </p>
          )}
          {grouped.map((group) => (
            <section key={group.category}>
              <p className="preset-picker-group" data-testid={`preset-group-${group.category}`}>
                {t(`providers.presetPicker.group.${group.category}`)}
              </p>
              <ul className="preset-picker-entries">
                {group.entries.map((preset) => (
                  <li key={preset.id}>
                    <button
                      className="preset-picker-entry"
                      data-testid={`preset-entry-${preset.id}`}
                      onClick={() => onPick(preset.id)}
                      title={preset.baseURL}
                    >
                      <Provider_icon
                        size="sm"
                        icon={preset.icon}
                        color={preset.iconColor}
                        name={preset.name}
                      />
                      <span className="preset-picker-text">
                        <span className="preset-picker-name">{preset.name}</span>
                        <span className="preset-picker-meta">
                          {preset.note ? `${preset.note} · ` : ''}
                          {preset.baseURL}
                        </span>
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            </section>
          ))}
        </div>
        <div className="modal-actions">
          <button className="btn" onClick={onClose} data-testid="preset-picker-cancel">
            {t('common.cancel')}
          </button>
        </div>
      </div>
    </div>
  );
}
