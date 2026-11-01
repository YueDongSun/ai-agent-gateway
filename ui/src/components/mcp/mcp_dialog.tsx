import { useEffect, useId, useRef, type ReactNode } from 'react';

interface Mcp_dialog_props {
  title: string;
  busy?: boolean;
  on_close: () => void;
  children: ReactNode;
  test_id: string;
}

export function Mcp_dialog({ title, busy = false, on_close, children, test_id }: Mcp_dialog_props) {
  const dialog_ref = useRef<HTMLElement>(null);
  const title_id = useId();
  useEffect(() => {
    const previous_focus = document.activeElement;
    dialog_ref.current?.focus();
    return () => {
      if (previous_focus instanceof HTMLElement && previous_focus.isConnected) previous_focus.focus();
      else [...document.querySelectorAll<HTMLElement>('[role="dialog"]')].filter((element) => element !== dialog_ref.current && element.getClientRects().length > 0).at(-1)?.focus();
    };
  }, []);
  useEffect(() => { if (busy) dialog_ref.current?.focus(); }, [busy]);
  return <div className="mcp_backdrop" onClick={() => { if (!busy) on_close(); }}>
    <section ref={dialog_ref} className="mcp_dialog" role="dialog" aria-modal="true" aria-labelledby={title_id}
      tabIndex={-1} data-testid={test_id} onClick={(event) => event.stopPropagation()} onKeyDown={(event) => {
        if (event.key === 'Escape' && !busy) { event.preventDefault(); on_close(); }
        if (event.key !== 'Tab') return;
        const controls = dialog_ref.current?.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), textarea:not(:disabled), select:not(:disabled), [tabindex="0"]');
        if (!controls?.length) { event.preventDefault(); return; }
        const first = controls[0]; const last = controls[controls.length - 1];
        if (event.shiftKey && (document.activeElement === first || document.activeElement === dialog_ref.current)) { event.preventDefault(); last.focus(); }
        else if (!event.shiftKey && (document.activeElement === last || document.activeElement === dialog_ref.current)) { event.preventDefault(); first.focus(); }
      }}>
      <div className="mcp_dialog_heading"><h2 id={title_id}>{title}</h2><button className="btn" aria-label="Close dialog" disabled={busy} onClick={on_close}>Close</button></div>
      {children}
    </section>
  </div>;
}
