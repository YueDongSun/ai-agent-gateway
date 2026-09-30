// AAG-owned preload for the managed DSH HTTP surface. It runs before DSH.
// This authenticates the loopback transport; it does not sandbox plugin code.
import http from 'node:http';
import { timingSafeEqual } from 'node:crypto';

const control_token = process.env.AAG_DSH_CONTROL_TOKEN ?? '';
const runtime_id = process.env.AAG_DSH_RUNTIME_ID ?? '';
const port = Number(process.env.AAG_DSH_PORT);
if (!/^[a-f0-9]{64}$/.test(control_token) || !runtime_id || !Number.isInteger(port) || port < 1 || port > 65535) {
  throw new Error('dsh:invalid_control_environment');
}
delete process.env.AAG_DSH_CONTROL_TOKEN;
delete process.env.AAG_DSH_RUNTIME_ID;
delete process.env.AAG_DSH_PORT;
const expected_host = '127.0.0.1:' + port;
const expected_origin = 'http://' + expected_host;
const original_emit = http.Server.prototype.emit;
const cookie_name = 'aag_dsh_session';
function equal_token(value) {
  if (typeof value !== 'string') return false;
  const candidate = Buffer.from(value);
  const expected = Buffer.from(control_token);
  return candidate.length === expected.length && timingSafeEqual(candidate, expected);
}
function authorized(request) {
  const bearer = request.headers.authorization;
  if (typeof bearer === 'string' && bearer.startsWith('Bearer ') && equal_token(bearer.slice(7))) return true;
  const cookie = request.headers.cookie;
  if (typeof cookie !== 'string') return false;
  return cookie.split(';').some((part) => {
    const [name, ...value] = part.trim().split('=');
    return name === cookie_name && equal_token(value.join('='));
  });
}
function trusted_origin(request) {
  const origin = request.headers.origin;
  if (origin !== undefined && origin !== expected_origin) return false;
  return !['cross-site', 'same-site'].includes(request.headers['sec-fetch-site']);
}
function reject_request(response, status = 403) {
  response.writeHead(status, { 'content-type': 'application/json', 'cache-control': 'no-store' });
  response.end('{"error":"dsh_access_denied"}');
}
http.Server.prototype.emit = function (event, ...args) {
  // Node dispatches Expect and CONNECT directly when handlers are registered;
  // neither path necessarily emits request. Authenticate before those handlers.
  if (!['request', 'upgrade', 'checkContinue', 'checkExpectation', 'connect'].includes(event)) return original_emit.call(this, event, ...args);
  // Do not change unrelated servers a plugin explicitly opens. Such plugins
  // retain their host privileges and require their own security review.
  const address = this.address();
  if (!address || typeof address === 'string' || address.port !== port) return original_emit.call(this, event, ...args);
  const [request, response_or_socket] = args;
  const valid_host = request.headers.host === expected_host;
  let request_url;
  try { request_url = new URL(request.url, expected_origin); } catch { /* Rejected below. */ }
  const valid_target = request_url?.origin === expected_origin;
  if (event === 'upgrade' || event === 'connect') {
    // The managed WebUI is not an HTTP tunnel proxy. Upgrade targets follow
    // the same origin validation as ordinary requests.
    if (event === 'connect' || !valid_host || !valid_target || !trusted_origin(request) || !authorized(request)) {
      response_or_socket.end('HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n');
      response_or_socket.destroy();
      return true;
    }
    return original_emit.call(this, event, ...args);
  }
  const response = response_or_socket;
  if (!valid_host || !trusted_origin(request)) { reject_request(response); return true; }
  if (!request_url) { reject_request(response, 400); return true; }
  if (!valid_target) { reject_request(response); return true; }
  if (request.method === 'GET' && request_url.pathname === '/__aag_bootstrap') {
    if (request_url.searchParams.getAll('token').length !== 1 || !equal_token(request_url.searchParams.get('token'))) {
      reject_request(response); return true;
    }
    response.writeHead(303, {
      location: '/',
      'set-cookie': cookie_name + '=' + control_token + '; HttpOnly; SameSite=Strict; Path=/',
      'cache-control': 'no-store',
      'referrer-policy': 'no-referrer',
    });
    response.end();
    return true;
  }
  if (!authorized(request)) { reject_request(response); return true; }
  response.setHeader('referrer-policy', 'no-referrer');
  response.setHeader('x-content-type-options', 'nosniff');
  if (request.method === 'GET' && request_url.pathname === '/__aag_health') {
    response.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' });
    response.end(JSON.stringify({ runtime_id, ready: true }));
    return true;
  }
  return original_emit.call(this, event, ...args);
};
