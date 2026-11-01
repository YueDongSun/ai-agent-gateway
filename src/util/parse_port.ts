/**
 * Parse user-supplied port text into a config-valid port number.
 * Same bounds as the gateway.port schema field (integer, 1..65535); every
 * caller that bypasses AppConfig.parse (CLI --port, interactive menu)
 * validates through this single helper instead of re-stating the bounds.
 * Returns null for anything the schema would reject.
 */
export function parse_port_input(raw: string): number | null {
  const port = Number(raw);
  if (!Number.isInteger(port) || port < 1 || port > 65535) return null;
  return port;
}
