import { useState } from 'react';
import { useT } from '../../../lib/ctx';
import { Provider_icon, provider_glyph_ids } from './provider_icon.js';

export interface IconPickerProps {
  /** Current icon value (glyph id, literal char, or empty). */
  icon: string;
  /** Tile color shared by the preview and every grid option. */
  color: string;
  /** Provider name for the monogram fallback preview. */
  name: string;
  /** Hand the chosen icon value back to the modal (empty = monogram). */
  onPick: (icon: string) => void;
}

/** Clickable glyph grid for the unified provider modal, replacing the
 *  bare datalist: the preview tile toggles a dropdown of every designed
 *  glyph rendered on the provider's own tile color, so the user picks the
 *  final look, not a name. The first option is the initials monogram
 *  (clears the field); the text input next to it still accepts custom
 *  emoji or letters. */
export function Icon_picker({ icon, color, name, onPick }: IconPickerProps) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const current = icon.trim();

  return (
    <div className="icon-picker">
      <button
        type="button"
        className="icon-picker-toggle"
        title={t('providers.iconPicker.toggle')}
        aria-label={t('providers.iconPicker.toggle')}
        aria-expanded={open}
        data-testid="icon-picker-toggle"
        onClick={() => setOpen((v) => !v)}
      >
        <Provider_icon icon={icon} color={color} name={name} />
      </button>
      {open && (
        <div
          className="icon-picker-grid"
          role="group"
          aria-label={t('providers.iconPicker.toggle')}
          data-testid="icon-picker-grid"
        >
          <button
            type="button"
            className={`icon-picker-option${current === '' ? ' selected' : ''}`}
            title={t('providers.iconPicker.monogram')}
            aria-label={t('providers.iconPicker.monogram')}
            aria-pressed={current === ''}
            data-testid="icon-picker-monogram"
            onClick={() => {
              onPick('');
              setOpen(false);
            }}
          >
            <Provider_icon icon="" color={color} name={name} />
          </button>
          {provider_glyph_ids().map((glyph) => (
            <button
              key={glyph}
              type="button"
              className={`icon-picker-option${current === glyph ? ' selected' : ''}`}
              title={glyph}
              aria-label={glyph}
              aria-pressed={current === glyph}
              data-testid={`icon-picker-glyph-${glyph}`}
              onClick={() => {
                onPick(glyph);
                setOpen(false);
              }}
            >
              <Provider_icon icon={glyph} color={color} name={name} />
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
