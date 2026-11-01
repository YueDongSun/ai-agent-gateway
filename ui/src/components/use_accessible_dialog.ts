import { useLayoutEffect, useRef } from 'react';

const focusable_selector = 'button, input, textarea, select, a[href], [tabindex]';
const modal_selector = '[role="dialog"][aria-modal="true"], [role="alertdialog"][aria-modal="true"]';

/** Own the focus and scroll lifetime of a mounted modal, including a picker
 * being replaced by its editor. Callbacks stay current without refocusing
 * the form whenever its controlled values change. */
export function use_accessible_dialog(on_close: () => void, busy = false) {
  const dialog_ref = useRef<HTMLDivElement>(null);
  const action_ref = useRef({ on_close, busy });

  useLayoutEffect(() => { action_ref.current = { on_close, busy }; });

  useLayoutEffect(() => {
    const dialog = dialog_ref.current;
    if (!dialog) return;
    const previous_focus = document.activeElement;
    const is_topmost = () => [...document.querySelectorAll(modal_selector)].at(-1) === dialog;
    const focusable_controls = () => [...dialog.querySelectorAll<HTMLElement>(focusable_selector)]
      .filter((control) => control.tabIndex >= 0 && !control.matches(':disabled, [hidden], [inert]') &&
        !control.closest('[hidden], [inert]') && control.getClientRects().length > 0 &&
        getComputedStyle(control).visibility !== 'hidden');
    const focus_first = () => (focusable_controls()[0] ?? dialog).focus({ preventScroll: true });

    // Each dialog owns one rule node. Nested dialogs can remove theirs in any
    // order without restoring another dialog's lock or losing preexisting styles.
    // Compensate the scrollbar once; an already locked document has zero gutter.
    const scroll_lock = document.createElement('style');
    const gutter = window.innerWidth - document.documentElement.clientWidth;
    const body_padding = Number.parseFloat(getComputedStyle(document.body).paddingRight) || 0;
    scroll_lock.textContent = `html { overflow: hidden !important; } body { padding-right: ${body_padding + gutter}px !important; }`;
    document.head.append(scroll_lock);
    focus_first();

    const on_keydown = (event: KeyboardEvent) => {
      if (!is_topmost() || event.defaultPrevented) return;
      if (event.key === 'Escape') {
        event.preventDefault();
        event.stopPropagation();
        if (!action_ref.current.busy) action_ref.current.on_close();
        return;
      }
      if (event.key !== 'Tab') return;
      const controls = focusable_controls();
      const first = controls[0];
      const last = controls.at(-1);
      if (!first || !last) {
        event.preventDefault();
        dialog.focus({ preventScroll: true });
      } else if (event.shiftKey && (document.activeElement === first || document.activeElement === dialog)) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && (document.activeElement === last || document.activeElement === dialog)) {
        event.preventDefault();
        first.focus();
      }
    };
    const on_focus = (event: FocusEvent) => {
      if (is_topmost() && event.target instanceof Node && !dialog.contains(event.target)) focus_first();
    };
    document.addEventListener('keydown', on_keydown);
    document.addEventListener('focusin', on_focus);
    return () => {
      document.removeEventListener('keydown', on_keydown);
      document.removeEventListener('focusin', on_focus);
      scroll_lock.remove();
      if (previous_focus instanceof HTMLElement && previous_focus.isConnected) {
        previous_focus.focus({ preventScroll: true });
      }
    };
  }, []);

  return dialog_ref;
}
