const settle_ms = 350;

export async function run_phase_23_regressions({ cdp, check_in_page, check, sleep }) {
  await check_in_page(cdp, check, 'route save preserves untouched backend routes', async () => {
    if (!window.__aagDemoIsDemo) return 'not-demo-page';
    const api = window.__aag_smoke_api;
    if ((await api.getProxyStatus()).running) return 'gateway-must-be-stopped-for-initial-save';
    const before = structuredClone(await api.listRoutes());
    const save_button = document.querySelector('[data-testid="route-save"]');
    if (!save_button) return 'no-save-button';
    save_button.click();
    await new Promise((resolve) => setTimeout(resolve, 500));
    const after = await api.listRoutes();
    return { ok: before.length > 0 && JSON.stringify(after) === JSON.stringify(before), detail: 'before=' + before.length + ' after=' + after.length };
  });

  await check_in_page(cdp, check, 'route hop save persists and UI restore returns backend baseline', async () => {
    const api = window.__aag_smoke_api;
    const baseline = structuredClone(await api.listRoutes());
    const route = baseline.find((candidate) => candidate.hops.length > 0);
    if (!route) return 'no-routable-route';
    let card = [...document.querySelectorAll('.route-card')].find((candidate) => candidate.querySelector('.route-card-title strong')?.textContent === route.alias);
    const add_button = [...(card?.querySelectorAll('.route-card-actions button') ?? [])].find((button) => /Add Hop|添加跳转/.test(button.textContent));
    if (!add_button) return 'no-add-hop-button';
    add_button.click();
    await new Promise((resolve) => setTimeout(resolve, 150));
    const save_button = document.querySelector('[data-testid="route-save"]');
    if (!save_button) return 'no-save-after-add';
    save_button.click();
    await new Promise((resolve) => setTimeout(resolve, 500));
    const saved = structuredClone(await api.listRoutes());
    const saved_route = saved.find((candidate) => candidate.alias === route.alias);
    card = [...document.querySelectorAll('.route-card')].find((candidate) => candidate.querySelector('.route-card-title strong')?.textContent === route.alias);
    const remove_button = [...(card?.querySelectorAll('.hop-remove-cell button') ?? [])].at(-1);
    if (!remove_button) return 'no-remove-added-hop';
    remove_button.click();
    await new Promise((resolve) => setTimeout(resolve, 150));
    const restore_button = document.querySelector('[data-testid="route-save"]');
    if (!restore_button) return 'no-save-after-remove';
    restore_button.click();
    await new Promise((resolve) => setTimeout(resolve, 500));
    const restored = await api.listRoutes();
    return {
      ok: saved_route?.hops.length === route.hops.length + 1 && JSON.stringify(restored) === JSON.stringify(baseline),
      detail: 'saved=' + (saved_route?.hops.length ?? 'missing') + ' baseline=' + route.hops.length + ' restored=' + restored.length,
    };
  });

  await check_in_page(cdp, check, 'batch delete stages and discard preserves full backend routes', async () => {
    const api = window.__aag_smoke_api;
    const baseline = structuredClone(await api.listRoutes());
    const selectors = [...document.querySelectorAll('.route-card .route-select input')];
    if (selectors.length < 2) return 'need-two-visible-routes:' + selectors.length;
    const visible_before = document.querySelectorAll('.route-card').length;
    window.confirm = () => true;
    selectors.slice(0, 2).forEach((input) => input.click());
    await new Promise((resolve) => setTimeout(resolve, 100));
    const delete_button = [...document.querySelectorAll('.route-toolbar-actions button')].find((button) => button.classList.contains('danger'));
    if (!delete_button) return 'no-batch-delete-button';
    delete_button.click();
    await new Promise((resolve) => setTimeout(resolve, 200));
    const staged_count = document.querySelectorAll('.route-card').length;
    const discard_button = document.querySelector('[data-testid="route-discard"]');
    if (!discard_button) return 'no-discard-button';
    discard_button.click();
    await new Promise((resolve) => setTimeout(resolve, 500));
    const restored = await api.listRoutes();
    return {
      ok: staged_count === visible_before - 2 && document.querySelectorAll('.route-card').length === visible_before && JSON.stringify(restored) === JSON.stringify(baseline),
      detail: 'visible=' + visible_before + ' staged=' + staged_count + ' backend=' + restored.length + ' baseline=' + baseline.length,
    };
  });

  await check_in_page(cdp, check, 'upstream enable disable enable persists backend state', async () => {
    const api = window.__aag_smoke_api;
    const baseline = structuredClone(await api.listProxyProviders());
    const provider = baseline[0];
    if (!provider) return 'no-provider';
    const toggle = document.querySelector('[data-testid="upstream-toggle-' + CSS.escape(provider.id) + '"]');
    if (!toggle) return 'no-upstream-toggle';
    toggle.click();
    await new Promise((resolve) => setTimeout(resolve, 500));
    const disabled = (await api.listProxyProviders()).find((candidate) => candidate.id === provider.id);
    document.querySelector('[data-testid="upstream-toggle-' + CSS.escape(provider.id) + '"]')?.click();
    await new Promise((resolve) => setTimeout(resolve, 500));
    const enabled = (await api.listProxyProviders()).find((candidate) => candidate.id === provider.id);
    return { ok: disabled?.enabled === !provider.enabled && enabled?.enabled === provider.enabled, detail: 'baseline=' + provider.enabled + ' disabled=' + disabled?.enabled + ' enabled=' + enabled?.enabled };
  });

  await check_in_page(cdp, check, 'upstream pin then unpin persists backend state', async () => {
    const api = window.__aag_smoke_api;
    const provider = (await api.listProxyProviders())[0];
    const pin_toggle = document.querySelector('.failover-flow .toggle-switch input');
    if (!provider || !pin_toggle) return 'no-pin-toggle';
    pin_toggle.click();
    await new Promise((resolve) => setTimeout(resolve, 300));
    const pinned = await api.getPinnedProvider();
    document.querySelector('.failover-flow .toggle-switch input')?.click();
    await new Promise((resolve) => setTimeout(resolve, 300));
    const unpinned = await api.getPinnedProvider();
    return { ok: pinned === provider.id && unpinned === null, detail: 'pin=' + pinned + ' unpin=' + unpinned };
  });

  const geometry_script = [
    "(() => {",
    "const first = document.querySelector('[data-testid^=\"hop-handle-\"][data-testid$=\"-0\"]');",
    "const alias = first?.closest('.hop-row')?.getAttribute('data-route-alias');",
    "const second = alias && document.querySelector('[data-testid=\"hop-handle-' + alias + '-1\"]');",
    "if (!first || !second || !alias) return null;",
    "first.scrollIntoView({ block: 'center' }); second.scrollIntoView({ block: 'center' });",
    "const source = first.getBoundingClientRect(); const target = second.getBoundingClientRect();",
    "const value = document.querySelector('.hop-row[data-route-alias=\"' + CSS.escape(alias) + '\"][data-hop-idx=\"0\"] select')?.value;",
    "return { alias, value, source: { x: source.x + source.width / 2, y: source.y + source.height / 2 }, target: { x: target.x + target.width / 2, y: target.y + target.height / 2 } };",
    "})()",
  ].join('');  const drag_geometry = await cdp.eval_value(geometry_script);
  if (!drag_geometry) {
    check('pointer hop drag reorders route', false, 'no-two-hop-geometry');
  } else {
    await cdp.send('Input.dispatchMouseEvent', { type: 'mousePressed', x: drag_geometry.source.x, y: drag_geometry.source.y, button: 'left', buttons: 1, clickCount: 1, pointerType: 'mouse' });
    await sleep(settle_ms);
    await cdp.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: drag_geometry.target.x, y: drag_geometry.target.y, button: 'left', buttons: 1, pointerType: 'mouse' });
    await sleep(settle_ms);
    await cdp.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: drag_geometry.target.x, y: drag_geometry.target.y, button: 'left', clickCount: 1, pointerType: 'mouse' });
    await sleep(settle_ms);
    const moved_value = await cdp.eval_value("document.querySelector('.hop-row[data-route-alias=\"" + drag_geometry.alias + "\"][data-hop-idx=\"1\"] select')?.value ?? ''");
    check('pointer hop drag reorders route', moved_value === drag_geometry.value, 'moved=' + moved_value + ' expected=' + drag_geometry.value);
    await cdp.eval_value("document.querySelector('[data-testid=\"route-discard\"]')?.click(); 1");
    await sleep(settle_ms);
  }
}
