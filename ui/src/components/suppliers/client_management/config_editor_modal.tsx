import { useEffect, useId, useMemo, useRef, useState } from 'react';
import { read_client_config, save_client_config } from '../../../lib/api/providers';
import { useT } from '../../../lib/ctx';
import { stripJsoncComments, validateJsoncDocument as validate_document } from '../../../lib/jsonc.js';
import { use_page_actions } from '../../page_actions.js';
import { use_accessible_dialog } from '../../use_accessible_dialog.js';

/** Debounce window for per-keystroke JSONC validation. Keep the editor
 *  responsive while avoiding a validation call on every character. */
const validation_debounce_ms = 300;

/** Validate JSONC; returns an error message or null when valid. Empty text
 *  is left for the server to reject (Save surfaces the real error). */
function validateDocument(text: string): string | null {
  if (!text.trim()) return null;
  return validate_document(text);
}

interface Config_editor_modal_props {
  app_type: string;
  onClose: () => void;
  /** Called after a successful save so the parent can refresh status. */
  on_saved: () => void;
}

/** cc-switch-style direct JSON/JSONC editor for the live client config
 *  file. Read is verbatim (comments survive), save validates then writes
 *  through the hardened atomic-write path with a backup. */
export function Config_editor_modal({ app_type, onClose, on_saved }: Config_editor_modal_props) {
  const t = useT();
  const [loading, set_loading] = useState(true);
  const [load_error, set_load_error] = useState<string | null>(null);
  const [target_path, set_target_path] = useState('');
  const [text, set_text] = useState('');
  const [baseline_text, set_baseline_text] = useState('');
  /** The most recently validated text remains visible during the debounce. */
  const [debounced_text, set_debounced_text] = useState('');
  const debounce_ref = useRef<number | null>(null);
  const [saving, set_saving] = useState(false);
  const saving_ref = useRef(false);
  const [save_error, set_save_error] = useState<string | null>(null);
  const [flash_saved, set_flash_saved] = useState(false);
  const title_id = useId();
  const dialog_ref = use_accessible_dialog(onClose, saving);
  /** Post-save auto-close timer; cancelled on unmount so a backdrop click
   *  during the 450 ms window cannot fire onClose after unmount. */
  const close_timer_ref = useRef<number | null>(null);
  useEffect(() => () => {
    if (close_timer_ref.current !== null) window.clearTimeout(close_timer_ref.current);
  }, []);

  useEffect(() => {
    let active = true;
    read_client_config(app_type)
      .then((target) => {
        if (!active) return;
        set_target_path(target.path);
        set_text(target.contents);
        set_baseline_text(target.contents);
        set_debounced_text(target.contents);
        set_loading(false);
      })
      .catch((error: unknown) => {
        if (!active) return;
        set_load_error(String(error));
        set_loading(false);
      });
    return () => {
      active = false;
    };
  }, [app_type]);

  /** Debounced text for validation — the editor updates immediately while the
   *  validator runs at most every validation_debounce_ms. */
  useEffect(() => {
    if (debounce_ref.current !== null) window.clearTimeout(debounce_ref.current);
    debounce_ref.current = window.setTimeout(() => {
      set_debounced_text(text);
      debounce_ref.current = null;
    }, validation_debounce_ms);
    return () => {
      if (debounce_ref.current !== null) window.clearTimeout(debounce_ref.current);
    };
  }, [text]);

  const validation_error = useMemo(() => validateDocument(debounced_text), [debounced_text]);
  const validation_pending = text !== debounced_text;

  /** Always validate the current editor contents synchronously before saving:
   *  a pending debounce must never authorize invalid text. */
  const handle_save = async () => {
    if (saving_ref.current || loading || load_error) return false;
    const current_validation_error = validateDocument(text);
    if (current_validation_error) {
      set_debounced_text(text);
      return false;
    }
    set_save_error(null);
    set_saving(true);
    saving_ref.current = true;
    try {
      await save_client_config(app_type, text);
      set_baseline_text(text);
      set_flash_saved(true);
      on_saved();
      if (close_timer_ref.current !== null) window.clearTimeout(close_timer_ref.current);
      close_timer_ref.current = window.setTimeout(() => {
        close_timer_ref.current = null;
        onClose();
      }, 450);
      return true;
    } catch (error: unknown) {
      set_save_error(String(error));
      return false;
    } finally {
      set_saving(false);
      saving_ref.current = false;
    }
  };

  use_page_actions({ target: `${app_type} configuration`, state: loading || saving ? 'busy' : load_error || save_error || validation_error ? 'error' : text !== baseline_text ? 'dirty' : 'clean',
    on_save: async () => { if (!await handle_save()) throw new Error('Client configuration save failed'); },
    save_disabled: loading || saving || Boolean(load_error || validation_error) || validation_pending || text === baseline_text,
  }, 20);

  const handle_format = () => {
    const current_validation_error = validateDocument(text);
    if (current_validation_error) {
      set_debounced_text(text);
      return;
    }
    try {
      const parsed: unknown = JSON.parse(stripJsoncComments(text));
      set_text(JSON.stringify(parsed, null, 2) + '\n');
    } catch {
      // Leave text untouched; the inline validator already reports it.
    }
  };

  return (
    <div className="modal-backdrop" onClick={() => { if (!saving) onClose(); }}>
      <div className="modal" ref={dialog_ref} role="dialog" aria-modal="true" aria-labelledby={title_id} aria-busy={saving} tabIndex={-1} onClick={(e) => e.stopPropagation()} data-testid="config-editor-modal">
        <h3 id={title_id}>
          {t('client_config.edit_title')} <code style={{ fontSize: '0.8em', overflowWrap: 'anywhere' }}>{target_path}</code>
        </h3>
        {loading && <p className="muted small">{t('client_config.edit_loading')}</p>}
        {load_error && (
          <p role="alert" className="error-banner" data-testid="config-editor-load-error">
            {t('client_config.edit_load_failed', { message: load_error })}
          </p>
        )}
        {!loading && !load_error && (
          <>
            <textarea
              className="input"
              rows={20}
              spellCheck={false}
              style={{ fontFamily: 'ui-monospace, Consolas, monospace' }}
              value={text}
              disabled={saving}
              onChange={(e) => set_text(e.target.value)}
              data-testid="config-editor-text"
            />
            {validation_error && (
              <p role="alert" className="error-banner small" data-testid="config-editor-invalid">
                {validation_error === 'root_not_object'
                  ? t('client_config.edit_root_not_object')
                  : t('client_config.edit_invalid_json', { message: validation_error })}
              </p>
            )}
            {save_error && (
              <p role="alert" className="error-banner small" data-testid="config-editor-save-error">
                {t('client_config.edit_save_failed', { message: save_error })}
              </p>
            )}
            <div className="modal-actions">
              <button className="btn" onClick={handle_format} disabled={validation_pending || Boolean(validation_error)}>
                {t('client_config.edit_format')}
              </button>
              <button className="btn" onClick={onClose} disabled={saving} data-testid="config_editor_cancel">
                {t('common.cancel')}
              </button>
              <button
                className="btn primary"
                onClick={handle_save}
                disabled={saving || Boolean(validation_error)}
                data-testid="config-editor-save"
              >
                {flash_saved ? t('providers.saved') : t('common.save')}
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
