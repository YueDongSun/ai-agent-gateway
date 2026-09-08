import { useEffect, useState } from 'react';

interface Secret_value_props {
  value: string;
  empty_label: string;
  hidden_label: string;
  show_label: string;
  hide_label: string;
  test_id: string;
}

export function Secret_value({
  value,
  empty_label,
  hidden_label,
  show_label,
  hide_label,
  test_id,
}: Secret_value_props) {
  const [revealed_value, set_revealed_value] = useState<string | null>(null);
  const is_revealed = Boolean(value) && revealed_value === value;

  useEffect(() => {
    set_revealed_value(null);
  }, [value]);

  return (
    <span className="secret_value">
      <code
        className="endpoint-value"
        aria-label={value && !is_revealed ? hidden_label : undefined}
        data-testid={`${test_id}_value`}
      >
        {!value ? empty_label : is_revealed ? value : '••••••••'}
      </code>
      {value && (
        <button
          type="button"
          className="btn small secret_toggle"
          aria-label={is_revealed ? hide_label : show_label}
          aria-pressed={is_revealed}
          data-testid={`${test_id}_visibility`}
          onClick={() => set_revealed_value(is_revealed ? null : value)}
        >
          {is_revealed ? hide_label : show_label}
        </button>
      )}
    </span>
  );
}
