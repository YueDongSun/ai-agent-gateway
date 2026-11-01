/**
 * <ModelSuggest /> — portal-rendered model name picker.
 *
 * Follows the cc-switch `ModelInputWithFetch` pattern:
 *   - Plain controlled `<input>` (typing → onChange; does NOT auto-open menu on focus).
 *   - Separate ▾ chevron button toggles the menu.
 *   - Menu rendered via `createPortal(document.body)` with `position: fixed`
 *     (immune to table/sticky-toolbar clipping and stacking context issues).
 *   - Item click → onChange + close. NO `inputRef.focus()` call
 *     (kills the focus→onFocus→setOpen race that broke the previous build).
 *   - Optional `onFetch` prop renders a ⤓ button for fetching upstream models.
 *
 * Keyboard (while input is focused and menu is open):
 *   ↓ / ↑   move highlight
 *   Enter    accept highlighted suggestion
 *   Escape   close menu
 */
import { createPortal } from 'react-dom';
import { useEffect, useMemo, useRef, useState } from 'react';

interface ModelSuggestProps {
  /** Current value (controlled). */
  value: string;
  /** Called when the user picks or types a value. */
  onChange: (next: string) => void;
  /** Candidate models to suggest. */
  options: string[];
  /** Placeholder text. */
  placeholder?: string;
  /** Visible label for screen readers / title. */
  ariaLabel?: string;
  /** Renders a ⤓ button beside the chevron; called on click. */
  onFetch?: () => void;
  /** When true the ⤓ button shows a spinner instead. */
  fetching?: boolean;
  /** Locks typing and picker actions during the owning editor operation. */
  disabled?: boolean;
}

export function ModelSuggest({
  value,
  onChange,
  options,
  placeholder,
  ariaLabel,
  onFetch,
  fetching,
  disabled = false,
}: ModelSuggestProps) {
  const [open, setOpen] = useState(false);
  const [highlight, setHighlight] = useState(-1);
  const [rect, setRect] = useState<DOMRect | null>(null);

  const wrapRef = useRef<HTMLDivElement | null>(null);
  const portalRef = useRef<HTMLUListElement | null>(null);

  // ---- filter ----
  const filtered = useMemo(() => {
    const q = value.trim().toLowerCase();
    if (!q) return options.slice(0, 80);
    const base = options.filter((o) => o.toLowerCase().includes(q)).slice(0, 80);
    // Keep the literal value so the user can submit custom names.
    if (!base.includes(value)) return [value, ...base];
    return base;
  }, [options, value]);

  // ---- open / close helpers ----
  const openMenu = () => {
    if (disabled) return;
    const el = wrapRef.current?.querySelector('input');
    if (el) setRect(el.getBoundingClientRect());
    setOpen(true);
    setHighlight(-1);
  };

  const closeMenu = () => {
    setOpen(false);
    setHighlight(-1);
  };

  const accept = (next: string) => {
    if (disabled) return;
    onChange(next);
    closeMenu();
    // NOTE: no inputRef.focus() — that was the bug (focus→onFocus→setOpen race).
  };

  useEffect(() => { if (disabled) closeMenu(); }, [disabled]);

  // ---- outside click (checks BOTH wrapper and portal) ----
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node;
      if (wrapRef.current?.contains(t)) return;   // inside the input/button
      if (portalRef.current?.contains(t)) return;  // inside the portal menu
      closeMenu();
    };
    document.addEventListener('mousedown', onDown);
    return () => document.removeEventListener('mousedown', onDown);
  }, [open]);

  // ---- close on scroll/resize (fixed coords go stale) ----
  useEffect(() => {
    if (!open) return;
    const close = () => closeMenu();
    window.addEventListener('scroll', close, { capture: true });
    window.addEventListener('resize', close);
    return () => {
      window.removeEventListener('scroll', close, { capture: true });
      window.removeEventListener('resize', close);
    };
  }, [open]);

  // ---- keep highlight in bounds ----
  useEffect(() => {
    if (open && highlight >= filtered.length) setHighlight(filtered.length - 1);
  }, [open, filtered, highlight]);

  // ---- keyboard ----
  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (disabled) return;
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (!open) openMenu();
      setHighlight((h) => (h + 1) % Math.max(filtered.length, 1));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (!open) openMenu();
      setHighlight((h) => (h <= 0 ? filtered.length - 1 : h - 1));
    } else if (e.key === 'Enter') {
      if (open && highlight >= 0 && highlight < filtered.length) {
        e.preventDefault();
        accept(filtered[highlight]);
      }
    } else if (e.key === 'Escape') {
      if (open) {
        e.preventDefault();
        closeMenu();
      }
    }
  };

  // ---- portal menu (position: fixed, coordinates from input rect) ----
  const menu =
    open && filtered.length > 0 && rect
      ? createPortal(
          <ul
            ref={portalRef}
            className="model-suggest-menu portal"
            role="listbox"
            style={{
              position: 'fixed',
              top: rect.bottom + 4,
              left: rect.left,
              width: rect.width,
            }}
          >
            {filtered.map((opt, i) => (
              <li
                key={opt}
                role="option"
                aria-selected={i === highlight}
                className={
                  i === highlight
                    ? 'model-suggest-item active'
                    : 'model-suggest-item'
                }
                onMouseDown={(e) => {
                  e.preventDefault(); // keep focus on input
                  accept(opt);
                }}
                onMouseEnter={() => setHighlight(i)}
              >
                {opt}
              </li>
            ))}
          </ul>,
          document.body,
        )
      : null;

  return (
    <div className="model-suggest" ref={wrapRef}>
      <input
        className="input"
        type="text"
        disabled={disabled}
        value={value}
        placeholder={placeholder}
        aria-label={ariaLabel ?? placeholder}
        autoComplete="off"
        spellCheck={false}
        onChange={(e) => {
          onChange(e.target.value);
          if (!open) openMenu();
          setHighlight(-1);
        }}
        // NO onFocus open — that was the bug class.
        onFocus={() => { if (open) setRect(wrapRef.current?.querySelector('input')?.getBoundingClientRect() ?? null); }}
        onKeyDown={onKeyDown}
      />
      {onFetch && (
        <button
          type="button"
          className="model-suggest-toggle"
          tabIndex={-1}
          disabled={disabled || fetching}
          title={fetching ? '…' : '⤓ fetch upstream models'}
          onMouseDown={(e) => { e.preventDefault(); if (!disabled) onFetch(); }}
        >
          {fetching ? '…' : '⤓'}
        </button>
      )}
      <button
        type="button"
        disabled={disabled}
        className="model-suggest-toggle"
        tabIndex={-1}
        aria-hidden="true"
        onMouseDown={(e) => {
          e.preventDefault(); // don't steal focus
          if (open) closeMenu(); else openMenu();
        }}
      >
        ▾
      </button>
      {menu}
    </div>
  );
}
