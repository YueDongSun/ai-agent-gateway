import { useEffect, useRef, useState } from 'react';
import type { Dsh_translate } from './dsh_i18n.js';
import { use_page_actions } from '../page_actions.js';

export interface Dsh_confirmation {
  title: string;
  changes: string[];
  warnings: string[];
  expires_at_ms?: number;
  blockers?: string[];
  can_confirm?: boolean;
  floating_action?: 'save' | 'delete';
}

interface Dsh_preview_dialog_props {
  preview: Dsh_confirmation;
  t: Dsh_translate;
  on_cancel: () => void;
  on_confirm: () => void;
  return_focus?: HTMLElement | null;
}

/** Keep keyboard focus in the review surface and restore its initiating control. */
export function Dsh_preview_dialog({ preview, t, on_cancel, on_confirm, return_focus }: Dsh_preview_dialog_props) {
  const dialog_ref = useRef<HTMLElement>(null);
  const [acknowledged, set_acknowledged] = useState(false);
  const [now, set_now] = useState(Date.now());
  const expired = preview.expires_at_ms !== undefined && preview.expires_at_ms <= now;
  const can_confirm = acknowledged && !expired && preview.can_confirm !== false && !preview.blockers?.length;
  const confirm = () => { if (can_confirm) on_confirm(); };
  use_page_actions({ target: `${preview.title} review`, state: expired || Boolean(preview.blockers?.length) ? 'error' : preview.floating_action ? 'dirty' : 'idle',
    on_save: preview.floating_action === 'save' ? confirm : undefined,
    on_delete: preview.floating_action === 'delete' ? confirm : undefined,
    save_disabled: !can_confirm, delete_disabled: !can_confirm,
  }, 30);

  useEffect(() => {
    const previous_focus = return_focus ?? document.activeElement;
    dialog_ref.current?.focus();
    const timer = window.setInterval(() => set_now(Date.now()), 1000);
    return () => {
      window.clearInterval(timer);
      if (previous_focus instanceof HTMLElement) previous_focus.focus();
    };
  }, []);

  return (
    <div className="dsh_modal_backdrop" onClick={on_cancel}>
      <section
        ref={dialog_ref}
        className="dsh_preview_dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="dsh_preview_title"
        tabIndex={-1}
        data-testid="dsh_preview_dialog"
        onClick={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if (event.key === 'Escape') on_cancel();
          if (event.key !== 'Tab') return;
          const focusable = dialog_ref.current?.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), [tabindex="0"]');
          if (!focusable?.length) return;
          const first = focusable[0];
          const last = focusable[focusable.length - 1];
          if (event.shiftKey && (document.activeElement === first || document.activeElement === dialog_ref.current)) {
            event.preventDefault();
            last.focus();
          } else if (!event.shiftKey && document.activeElement === last) {
            event.preventDefault();
            first.focus();
          }
        }}
      >
        <span className="dsh_eyebrow">{t('preview')}</span>
        <h2 id="dsh_preview_title">{preview.title}</h2>
        {Boolean(preview.blockers?.length) && <div className="dsh_notice dsh_error dsh_setup_blockers" role="alert" data-testid="dsh_setup_blockers"><h3>{t('setup_blockers')}</h3><ul>{preview.blockers?.map((blocker, index) => <li key={index}>{blocker}</li>)}</ul></div>}
        <h3>{t('changes')}</h3>
        <ul>{preview.changes.map((change, index) => <li key={index}>{change}</li>)}</ul>
        <div className="dsh_notice dsh_warning">
          <h3>{t('risks')}</h3>
          <ul>{preview.warnings.map((warning, index) => <li key={index}>{warning}</li>)}</ul>
        </div>
        {preview.expires_at_ms !== undefined && <p className="dsh_hint">{t('expiry')}: {new Date(preview.expires_at_ms).toLocaleTimeString()}</p>}
        {expired && <p className="dsh_error" role="alert">{t('expired')}</p>}
        <label className="dsh_acknowledgement">
          <input type="checkbox" checked={acknowledged} onChange={(event) => set_acknowledged(event.target.checked)} data-testid="dsh_acknowledge" />
          <span>{t('acknowledge')}</span>
        </label>
        <div className="dsh_actions">
          <button type="button" className="btn" onClick={on_cancel} data-testid="dsh_cancel_preview">{t('cancel_preview')}</button>
          <button type="button" className="btn primary" onClick={confirm} disabled={!can_confirm} data-testid="dsh_confirm">{t('confirm')}</button>
        </div>
      </section>
    </div>
  );
}
