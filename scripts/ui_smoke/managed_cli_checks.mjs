/** Browser-demo checks exercise UI and synthetic IPC only. */
export async function run_managed_cli_checks({ cdp, check_in_page, check }) {
  await check_in_page(cdp, check, 'Gemini, Kilo and Cline navigation have visible distinct icons', () => {
    const buttons = ['gemini', 'kilo_cli', 'cline_cli'].map((app_type) =>
      document.querySelector(`[data-app="${app_type}"]`));
    const icons = buttons.map((button) => button?.querySelector('svg'));
    return { ok: icons.every((icon) => icon && icon.getBoundingClientRect().width > 0)
      && new Set(icons.map((icon) => icon?.innerHTML)).size === 3 };
  });

  await check_in_page(cdp, check, 'Suppliers retains working Kilo and Cline synthetic launchers', async () => {
    const internals = window.__TAURI_INTERNALS__;
    const original_invoke = internals.invoke;
    const launches = [];
    internals.invoke = async (command, args) => {
      if (command === 'managed_cli_open_terminal') launches.push(args.appType);
      return original_invoke(command, args);
    };
    try {
      for (const app_type of ['kilo_cli', 'cline_cli']) {
        await window.__aag_open_client_management(app_type);
        let launcher;
        for (let attempt = 0; attempt < 30; attempt += 1) {
          await new Promise((resolve) => setTimeout(resolve, 100));
          launcher = document.querySelector('[data-testid="managed-cli-open"]');
          if (launcher && !launcher.disabled && document.querySelector(`[data-testid="client-settings-tab-${app_type}"]`)?.getAttribute('aria-pressed') === 'true') break;
        }
        if (!launcher || launcher.disabled) return `launcher-unavailable:${app_type}`;
        launcher.click();
        for (let attempt = 0; attempt < 20 && !launches.includes(app_type); attempt += 1) {
          await new Promise((resolve) => setTimeout(resolve, 100));
        }
      }
      return { ok: launches.includes('kilo_cli') && launches.includes('cline_cli') };
    } finally {
      internals.invoke = original_invoke;
      await window.__aag_open_client_management('claude_code');
      for (let attempt = 0; attempt < 30; attempt += 1) {
        await new Promise((resolve) => setTimeout(resolve, 100));
        if (document.querySelector('[data-testid="claude-open-cli"]')) break;
      }
    }
  });

  await check_in_page(cdp, check, 'missing CLI shows its official installation link', async () => {
    const internals = window.__TAURI_INTERNALS__;
    const original_invoke = internals.invoke;
    internals.invoke = (command, args) => command === 'managed_cli_status'
      ? Promise.resolve({ appType: args.appType, available: false, binary: null,
          installUrl: 'https://kilo.ai/docs/code-with-ai/platforms/cli' })
      : original_invoke(command, args);
    try {
      await window.__aag_open_client_management('kilo_cli');
      for (let attempt = 0; attempt < 30; attempt += 1) {
        await new Promise((resolve) => setTimeout(resolve, 100));
        const link = document.querySelector('[data-testid="managed-cli-install"]');
        if (link) return { ok: link.href === 'https://kilo.ai/docs/code-with-ai/platforms/cli'
          && document.querySelector('[data-testid="managed-cli-open"]')?.disabled === true };
      }
      return 'missing-install-link';
    } finally {
      internals.invoke = original_invoke;
      await window.__aag_open_client_management('claude_code');
      for (let attempt = 0; attempt < 30; attempt += 1) {
        await new Promise((resolve) => setTimeout(resolve, 100));
        if (document.querySelector('[data-testid="claude-open-cli"]')) break;
      }
    }
  });
}
