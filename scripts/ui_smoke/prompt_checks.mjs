/** Browser-demo evidence for the prompt-enhancement page.
 *
 * The page owns the two-stage loop, so these checks exercise the real click
 * path against the synthetic IPC backend. Native pipeline behavior (target
 * resolution, validation, template overrides, output normalization) is covered
 * by Rust unit tests in src-tauri/src/services/prompt.
 *
 * The checks always enter the page through its own navigation button and return
 * to the Claude page afterwards, so they cannot inherit page state from an
 * earlier scenario.
 */
export async function run_prompt_checks({ cdp, check_in_page, check }) {
  await cdp.eval_value(`(() => {
    if (window.__aagDemoIsDemo !== true) throw new Error('prompt smoke requires synthetic IPC');
    const element = (id) => document.querySelector('[data-testid="' + id + '"]');
    const wait_for = async (predicate) => {
      const deadline = Date.now() + 5000;
      while (Date.now() < deadline) {
        const value = await predicate();
        if (value) return value;
        await new Promise((resolve) => setTimeout(resolve, 40));
      }
      throw new Error('prompt UI condition timed out');
    };
    const set_text = async (id, value) => {
      const target = await wait_for(() => element(id));
      const prototype = target instanceof HTMLTextAreaElement
        ? HTMLTextAreaElement.prototype
        : HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(prototype, 'value').set.call(target, value);
      target.dispatchEvent(new Event('input', { bubbles: true }));
      await new Promise((resolve) => setTimeout(resolve, 60));
    };
    const click = async (id) => {
      const target = await wait_for(() => {
        const item = element(id);
        return item && !item.disabled ? item : null;
      });
      target.focus();
      target.click();
      await new Promise((resolve) => setTimeout(resolve, 60));
    };
    window.__aag_prompt_smoke = { element, wait_for, set_text, click };
    return true;
  })()`);

  try {
    await check_in_page(cdp, check, 'prompt page mounts from the shell and offers local model backends', async () => {
      const api = window.__aag_prompt_smoke;
      await api.click('prompt_nav');
      await api.wait_for(() => api.element('prompt_page'));
      const select = await api.wait_for(() => {
        const node = api.element('prompt_target');
        return node && node.options.length > 0 ? node : null;
      });
      const groups = [...select.querySelectorAll('optgroup')].length;
      return { ok: select.options.length > 0, detail: 'backends=' + select.options.length + ' groups=' + groups };
    });

    await check_in_page(cdp, check, 'both stages run and the Chinese brief carries the source requirement', async () => {
      const api = window.__aag_prompt_smoke;
      await api.set_text('prompt_input', '写一个登录页');
      await api.click('prompt_run');
      const draft = await api.wait_for(() => {
        const node = api.element('prompt_draft');
        return node && node.value.includes('写一个登录页') ? node : null;
      });
      const output = await api.wait_for(() => {
        const node = api.element('prompt_output');
        return node && node.value.trim().length > 0 ? node : null;
      });
      return {
        ok: draft.value.length > 0 && output.value.length > 0,
        detail: 'draft=' + draft.value.length + ' output=' + output.value.length,
      };
    });

    await check_in_page(cdp, check, 'editing the Chinese brief changes what the English stage receives', async () => {
      const api = window.__aag_prompt_smoke;
      const edited_marker = 'EDITED-BRIEF-MARKER';
      await api.set_text('prompt_draft', edited_marker);
      await api.click('prompt_optimize');
      const output = await api.wait_for(() => {
        const node = api.element('prompt_output');
        return node && node.value.includes(edited_marker) ? node : null;
      });
      return { ok: output.value.includes(edited_marker), detail: 'output=' + output.value.length };
    });

    await check_in_page(cdp, check, 'generated pair is recorded in browser history and copies through the clipboard', async () => {
      const api = window.__aag_prompt_smoke;
      const entry = await api.wait_for(() => api.element('prompt_history_item'));
      if (!entry) return 'no-history-entry';
      const output = api.element('prompt_output').value;
      const original = Object.getOwnPropertyDescriptor(navigator.clipboard, 'writeText');
      let copied = null;
      Object.defineProperty(navigator.clipboard, 'writeText', {
        configurable: true,
        value: async (value) => { copied = value; },
      });
      try {
        await api.click('prompt_copy_en');
      } finally {
        if (original) Object.defineProperty(navigator.clipboard, 'writeText', original);
        else delete navigator.clipboard.writeText;
      }
      return {
        ok: Boolean(entry) && copied === output && Boolean(api.element('prompt_notice')),
        detail: 'history=true copied=' + (copied === output) + ' notice=' + Boolean(api.element('prompt_notice')),
      };
    });
  } finally {
    // Later scenarios start from the Claude page; never leave the shell here.
    await cdp.eval_value("window.__aag_open_client_management('claude_code')");
  }
}
