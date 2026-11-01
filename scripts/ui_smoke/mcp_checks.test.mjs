import assert from 'node:assert/strict';
import test from 'node:test';
import { install_mcp_smoke_helpers } from './mcp_checks.mjs';

test('MCP click and input finish when browser timers are suspended', async () => {
  const previous = {
    window: globalThis.window,
    document: globalThis.document,
    set_timeout: globalThis.setTimeout,
    input: globalThis.HTMLInputElement,
    text_area: globalThis.HTMLTextAreaElement,
    event: globalThis.Event,
    mutation_observer: globalThis.MutationObserver,
  };
  let timer_calls = 0;
  let focus_count = 0;
  let click_count = 0;
  let input_event_count = 0;
  class Test_input {
    set value(next) { this.current_value = next; }
    dispatchEvent(event) { if (event.type === 'input') input_event_count += 1; }
  }
  const input = new Test_input();
  const button = { disabled: true, focus: () => { focus_count += 1; }, click: () => { click_count += 1; } };
  const observers = new Set();
  class Test_mutation_observer {
    constructor(callback) { this.callback = callback; }
    observe() { observers.add(this); }
    disconnect() { observers.delete(this); }
  }
  try {
    globalThis.window = { __aagDemoIsDemo: true, __TAURI_INTERNALS__: { invoke: async () => null } };
    globalThis.document = {
      documentElement: {},
      querySelector: (selector) => selector.includes('mcp_save') ? button : selector.includes('mcp_id') ? input : null,
    };
    globalThis.MutationObserver = Test_mutation_observer;
    globalThis.HTMLInputElement = Test_input;
    globalThis.HTMLTextAreaElement = class Test_text_area {};
    globalThis.Event = class Test_event { constructor(type) { this.type = type; } };
    globalThis.setTimeout = () => { timer_calls += 1; return 0; };
    install_mcp_smoke_helpers();
    const actions = Promise.all([
      window.__aag_mcp_smoke.click('mcp_save'),
      window.__aag_mcp_smoke.input('mcp_id', 'smoke_mcp'),
    ]);
    await Promise.resolve();
    assert.equal(observers.size, 1, 'the initial false predicate must subscribe to changes');
    button.disabled = false;
    for (const observer of observers) observer.callback([]);
    await Promise.race([
      actions,
      new Promise((_, reject) => previous.set_timeout(() => reject(new Error('MCP actions waited on a suspended browser timer')), 100)),
    ]);
    assert.equal(focus_count, 1);
    assert.equal(click_count, 1);
    assert.equal(input.current_value, 'smoke_mcp');
    assert.equal(input_event_count, 1);
    assert.equal(timer_calls, 0);
    assert.equal(observers.size, 0, 'settled waits release their observers');
    const pending = window.__aag_mcp_smoke.wait_for(() => false);
    await Promise.resolve();
    assert.equal(observers.size, 1);
    window.__aag_mcp_smoke.restore();
    await assert.rejects(pending, /restored before condition settled/);
    assert.equal(observers.size, 0, 'restoring releases pending waits');
  } finally {
    globalThis.window?.__aag_mcp_smoke?.restore();
    for (const [key, value] of Object.entries({
      window: previous.window, document: previous.document, setTimeout: previous.set_timeout,
      HTMLInputElement: previous.input, HTMLTextAreaElement: previous.text_area, Event: previous.event,
      MutationObserver: previous.mutation_observer,
    })) {
      if (value === undefined) delete globalThis[key]; else globalThis[key] = value;
    }
  }
});
