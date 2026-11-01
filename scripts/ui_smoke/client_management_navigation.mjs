/** Navigate through the product's real Suppliers disclosure. This helper only
 * clicks UI controls; it never substitutes management state or IPC results. */
export async function install_client_management_navigation(cdp) {
  await cdp.eval_value(`(() => {
    if (window.__aagDemoIsDemo !== true) throw new Error('client navigation requires synthetic IPC');
    window.__aag_open_client_management = async (app_type = 'claude_code') => {
      const element = (id) => document.querySelector('[data-testid="' + id + '"]');
      const wait_for = async (read) => {
        for (let attempt = 0; attempt < 80; attempt++) {
          const value = read();
          if (value) return value;
          await new Promise((resolve) => setTimeout(resolve, 50));
        }
        throw new Error('Suppliers client management did not become ready');
      };
      if (!element('suppliers_page')) {
        const navigation = element('suppliers_nav');
        if (!navigation) throw new Error('Suppliers navigation is missing');
        navigation.click();
      }
      const inventory = element('supplier-section-inventory'); if (inventory && inventory.getAttribute('aria-selected') !== 'true') inventory.click();
      const toggle = await wait_for(() => element('client-management-toggle'));
      if (toggle.getAttribute('aria-expanded') !== 'true') toggle.click();
      const button = await wait_for(() => element('client-settings-tab-' + app_type));
      if (button.getAttribute('aria-pressed') !== 'true') button.click();
      await wait_for(() => document.querySelector('.client_management_body .page-header h2')?.textContent === button.textContent.trim()
        && element('providers-panel'));
      return true;
    };
    return true;
  })()`);
}

export async function open_client_management(cdp, app_type = 'claude_code') {
  await cdp.eval_value(`window.__aag_open_client_management(${JSON.stringify(app_type)})`);
}
