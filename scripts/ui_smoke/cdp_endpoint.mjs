export const cdp_timeout_ms = 10_000;

/** Read a local CDP endpoint without allowing a stalled response body to block a smoke run. */
export async function read_cdp_endpoint(cdp_origin, endpoint_path, { timeout_ms = cdp_timeout_ms } = {}) {
  const endpoint_url = new URL(endpoint_path, cdp_origin);
  const response = await fetch(endpoint_url, { signal: AbortSignal.timeout(timeout_ms) });
  if (!response.ok) throw new Error('CDP endpoint failed: ' + response.status);
  return response.json();
}
