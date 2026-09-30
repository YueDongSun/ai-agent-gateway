import { cdp_timeout_ms, read_cdp_endpoint } from './cdp_endpoint.mjs';

const command_timeout_ms = cdp_timeout_ms;

export class cdp_client {
  constructor(web_socket) {
    this.web_socket = web_socket;
    this.next_id = 0;
    this.pending = new Map();
    this.exceptions = [];
    this.console_errors = [];
    web_socket.addEventListener('message', (event) => this.on_message(event));
    web_socket.addEventListener('close', () => {
      for (const settle of this.pending.values()) {
        settle({ error: { message: 'CDP connection closed' } });
      }
      this.pending.clear();
    });
  }

  on_message(event) {
    const message = JSON.parse(event.data);
    const settle = this.pending.get(message.id);
    if (settle) {
      this.pending.delete(message.id);
      settle(message);
      return;
    }
    if (message.method === 'Runtime.exceptionThrown') {
      const details = message.params.exceptionDetails;
      this.exceptions.push(details.exception?.description ?? details.text ?? 'unknown');
    }
    if (message.method === 'Runtime.consoleAPICalled' && message.params.type === 'error') {
      this.console_errors.push(
        message.params.args.map((argument) => argument.value ?? argument.description ?? '').join(' '),
      );
    }
  }

  connect() {
    if (this.web_socket.readyState === WebSocket.OPEN) return Promise.resolve();
    return new Promise((resolve, reject) => {
      const cleanup = () => {
        clearTimeout(timer);
        this.web_socket.removeEventListener('open', on_open);
        this.web_socket.removeEventListener('error', on_failure);
        this.web_socket.removeEventListener('close', on_failure);
      };
      const on_open = () => { cleanup(); resolve(); };
      const on_failure = () => { cleanup(); reject(new Error('CDP connection failed')); };
      const timer = setTimeout(() => {
        cleanup();
        this.web_socket.close();
        reject(new Error('CDP connection timed out'));
      }, command_timeout_ms);
      this.web_socket.addEventListener('open', on_open, { once: true });
      this.web_socket.addEventListener('error', on_failure, { once: true });
      this.web_socket.addEventListener('close', on_failure, { once: true });
    });
  }

  send(method, params = {}) {
    return new Promise((resolve, reject) => {
      const id = ++this.next_id;
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error('CDP ' + method + ' timed out after ' + command_timeout_ms + 'ms'));
      }, command_timeout_ms);
      this.pending.set(id, (message) => {
        clearTimeout(timer);
        if (message.error) reject(new Error('CDP ' + method + ': ' + message.error.message));
        else resolve(message);
      });
      try {
        this.web_socket.send(JSON.stringify({ id, method, params }));
      } catch (error) {
        clearTimeout(timer);
        this.pending.delete(id);
        reject(error);
      }
    });
  }

  async eval(expression) {
    const exception_count = this.exceptions.length;
    const response = await this.send('Runtime.evaluate', {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (response.result?.exceptionDetails) {
      const details = response.result.exceptionDetails;
      throw new Error(
        (details.text || 'Uncaught') + ': ' +
        (details.exception?.description?.split('\n')[0] ?? '') +
        '\n  expression head: ' + expression.slice(0, 120),
      );
    }
    return {
      __async_error: this.exceptions.slice(exception_count).at(-1) ?? null,
      value: response.result?.result?.value,
    };
  }

  async eval_value(expression) {
    return (await this.eval(expression)).value;
  }
}

export async function attach_page(cdp_origin) {
  const page = (await read_cdp_endpoint(cdp_origin, '/json/list', { timeout_ms: command_timeout_ms }))
    .find((target) => target.type === 'page');
  if (!page) throw new Error('no page target in CDP list');
  const client = new cdp_client(new WebSocket(page.webSocketDebuggerUrl));
  await client.connect();
  await client.send('Runtime.enable');
  await client.send('Page.enable');
  await client.send('DOM.enable');
  return client;
}
