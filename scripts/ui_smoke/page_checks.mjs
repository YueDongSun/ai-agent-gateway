export async function check_in_page(cdp, check, name, page_function) {
  const envelope = await cdp.eval(`(${page_function.toString()})()`);
  const value = envelope.value;
  const async_detail = envelope.__async_error ? `async: ${envelope.__async_error.slice(0, 160)}` : '';
  const passed = value === 'ok' || (value !== null && typeof value === 'object' && value.ok === true);
  const value_detail = value === undefined || value === null
    ? 'eval returned no value'
    : typeof value === 'object'
      ? JSON.stringify(value)
      : String(value);
  check(name, passed && !envelope.__async_error, passed ? async_detail : [value_detail, async_detail].filter(Boolean).join(' | '));
  return passed && !envelope.__async_error;
}
