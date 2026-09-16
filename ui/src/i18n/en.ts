/// English message catalog. Keys are flat dot-namespaced; values are strings
/// or `{var}` interpolations. Add keys in lockstep with zh.ts.

export const en = {
  // App shell
  'app.brand': 'AI Agent Gateway',
  'app.subtitle': 'v{version} · {phase}',
  'app.footer': 'Local gateway with per-app failover · OpenAI Responses + Anthropic + Chat Completions',
  'app.error': 'Error: {message}',
  'app.update.placeholder': 'https://releases.example.com/aag/latest.json (optional)',
  'app.update.check': 'Check for update',
  'app.update.checking': 'Checking…',
  'app.update.available': 'Update available: v{version}',
  'app.update.none': 'No update available.',
  'app.update.failed': 'Update check failed: {message}',

  // Gateway status card (all apps)
  'app.gateway.title': 'AAG Gateway',
  'app.gateway.url': 'Base URL',
  'app.gateway.apiKey': 'API Key',
  'app.gateway.copy': 'Copy',
  'app.gateway.noKey': '(not set - proxy accepts any key)',
  'app.gateway.start': 'Start',
  'app.gateway.stop': 'Stop',
  'app.gateway.restarted': 'Gateway restarted — routes applied.',
  'app.gateway.portFallback': 'Port was in use — proxy moved to port {port} and saved it as the new default.',
  'app.gateway.portConflict': 'Could not start the gateway: every nearby port is taken. Free a port or change the base URL, then retry. Details: {message}',
  'app.gateway.editSettings': 'Gateway settings (bind host, port, API key)',
  'app.gateway.hostLabel': 'Bind host',
  'app.gateway.invalidHost': 'Bind host must be localhost, an IPv4 address, or an IPv6 literal.',
  'app.gateway.portLabel': 'Port',
  'app.gateway.invalidPort': 'Port must be an integer between 1 and 65535.',
  'app.gateway.savedRestartHint': 'Saved. Restart the gateway for the new settings to take effect.',
  'app.gateway.running': '● running',
  'app.gateway.stopped': '○ stopped',
  'app.gateway.ccswitchHint': 'In cc-switch, create an Anthropic provider with the Base URL and API Key below; requests fail over through this app\'s chain.',

  // Endpoint surface hint per app
  'app.endpoint.claude': 'AAG is cc-switch\'s upstream. Point cc-switch at the endpoint below; requests fail over through the chain.',
  'app.endpoint.codex': 'Codex speaks the OpenAI Responses API natively (/v1/responses). Start the gateway, then switch providers below.',
  'app.endpoint.chat': 'OpenAI Chat Completions surface (/v1/chat/completions). Requests fail over through this app\'s chain in list order.',

  // Provider chain (cc-switch style, all apps)
  'chain.title': 'Providers',
  'chain.desc': 'List order = failover order. The current provider is the chain head; a failing hop falls through to the next. Drag the ⋮⋮ handle to reorder.',
  'chain.add': '+ Add provider',
  'chain.empty': 'No providers yet. Add one or import from cc-switch.',
  'chain.current': 'CURRENT',
  'chain.out': 'OUT',
  'chain.enable': 'Enable',
  'chain.inUse': 'In Use',
  'chain.toggle.in': '+ chain',
  'chain.toggle.out': '− chain',
  'chain.toggle.help': 'Include / exclude this provider from the failover chain',
  'chain.dragHint': 'Drag to reorder (list order = failover order)',
  'chain.switched': 'Switched to {name}.',
  'chain.switched.wrote': 'Switched to {name} (wrote {count} file(s)).',
  'chain.delete.confirm': 'Delete provider "{id}"?',
  'chain.modal.add': 'Add provider',
  'chain.modal.edit': 'Edit provider: {name}',
  'chain.modal.name': 'Name',
  'chain.importCcSwitch': 'Import from cc-switch',
  'chain.importCcSwitch.report': 'Imported {imported} · Skipped {skipped} (already present) · Source had {total}.',
  'chain.importCcSwitch.empty': 'No providers found in cc-switch (~/.cc-switch/cc-switch.db).',
  'chain.importCcSwitch.failed': 'cc-switch import failed: {message}',
  'chain.importCcSwitch.routes': 'routes auto-seeded: {n}',
  'chain.importCcSwitch.noKey': 'skipped no-key: {n}',

  // Upstream Providers pool (Claude page)
  'claude.upstream.title': 'Upstream Providers',
  'claude.upstream.listTitle': 'Upstream List',
  'claude.upstream.add': '+ Add upstream',
  'claude.upstream.desc': 'Manage upstream API providers (DeepSeek, OpenAI, Anthropic, etc.). Use the row toggle to pull an upstream out of the failover pool; its model list feeds the hop-model suggestions in the routes below.',
  'claude.upstream.empty': 'No upstream providers. Add one to build the failover pool.',
  'claude.upstream.modal.add': 'Add upstream provider',
  'claude.upstream.modal.edit': 'Edit upstream: {name}',
  'claude.upstream.modal.id': 'ID',
  'claude.upstream.modal.name': 'Name',
  'claude.upstream.modal.type': 'Type',
  'claude.upstream.modal.baseUrl': 'Base URL (endpoint)',
  'claude.upstream.modal.apiKey': 'API key',
  // Phase 22r cleanup: the retired `claude.upstream.modal.modelMapping.*`
  // keys (5) are gone — the route editor absorbed model mapping (Phase 18).
  // Round 1 cleanup: `claude.upstream.pin` / `unpin` (no JSX consumer — the
  // pin toggle uses `pin.help` directly) and the entire `claude.mapping.*`
  // block (10 keys) were the retired model-mapping editor from before Phase
  // 18 absorbed model mapping into the route editor.
  'claude.upstream.pin.help': 'Force all requests through this provider (bypass failover chain)',
  'claude.upstream.pinned': 'Pinned to {name}. All requests now route through this provider.',
  'claude.upstream.unpinned': 'Unpinned. Normal failover restored.',
  'claude.upstream.pinnedTag': 'PINNED',
  'claude.upstream.activeTag': 'ACTIVE',

  // Failover route editor (shared by Claude + Codex)
  'route.title': 'Failover Routes',
  'route.desc': 'Each route maps a client-facing model name to an ordered failover chain. Drag the ⋮⋮ handle to reorder hops; remove individual providers from the chain. Save, then use "Restart proxy to apply" (while the gateway is running) to pick changes up.',
  'route.empty': 'No routes configured. Import from cc-switch or add upstream providers to auto-generate routes.',
  'route.add': '+ Add Route',
  'route.rename': 'Rename',
  'route.rename.prompt': 'New alias for "{alias}":',
  'route.renamed': 'Route renamed: {from} → {to}.',
  'route.exists': 'Route "{alias}" already exists.',
  'route.addHop': '+ Add Hop',
  'route.delete.confirm': 'Delete route "{alias}"?',
  'route.deleteSelected': 'Delete {n} selected',
  'route.deleteSelectedConfirm': 'Delete {n} selected routes?',
  'route.selectForBatch': 'Select for batch delete',
  'route.hopProvider': 'Provider',
  'route.hopModel': 'Upstream Model',
  'route.hopDragHint': 'Drag to reorder (↑/↓/Home/End when focused)',
  'route.hopCount': '{n} hop(s)',
  'route.noHops': 'No hops yet — click "+ Add Hop" to start the chain.',
  'route.expand': 'Expand hops',
  'route.collapse': 'Collapse hops',
  'route.saved': 'Routes saved. Restart the proxy to apply.',
  'route.restartHint': 'Routes are applied when the proxy starts. Restart the proxy to apply changes.',
  'route.restartNow': 'Restart proxy to apply',
  'claude.upstream.modal.models': 'Models (comma/newline separated; feeds hop-model suggestions)',
  'claude.upstream.disabledTag': 'DISABLED',
  'claude.upstream.enabled.help': 'Enable this upstream (failover chains skip disabled ones)',
  'claude.upstream.modelCount': '{n} models',
  'route.clientsRequest': 'clients request this name',
  'route.dirty': 'Unsaved changes',
  'route.discard': 'Discard',
  'route.fetchModels.empty': 'No models returned from provider "{provider}".',
  'route.fetchModels.failed': 'Fetch upstream models failed: {message}',

  // Provider test
  'claude.upstream.test': 'Test',
  'claude.upstream.test.help': 'Send a minimal request to test connectivity and model availability',
  'claude.upstream.test.ok': '✓ Connected ({latency}ms) · model: {model}',
  'claude.upstream.test.fail': '✗ Failed: {error}',

  // Codex mirror + launchers
  'codex.mirror.home': 'Mirror CODEX_HOME',
  'codex.mirror.notInitialized': 'not initialized - switch a provider first',

  // (Phase 22r cleanup: the retired modelCatalog / modelMapping editor
  // keys were removed — the route editor replaced that editor.)
  'codex.open.title': 'Open Codex',
  'codex.open.desktop': 'Open Codex Desktop (mirror)',
  'codex.open.desktop.help': 'Launches a NEW Codex Desktop instance pointed at AAG\'s gateway (auto-written config). The proxy handles failover. Your existing Codex Desktop stays untouched.',
  'codex.open.cli': 'Open Codex CLI in terminal',
  'codex.open.cli.help': 'Launches the Codex CLI in a terminal with CODEX_HOME + outbound proxy injected.',
  'codex.open.copilot': 'Open Copilot CLI in terminal',
  'codex.open.desktopLaunched': 'Codex Desktop parasite launched (CODEX_HOME={dir}). Native Codex Desktop keeps running untouched.',
  'codex.open.cliLaunched': 'Codex CLI started in terminal (CODEX_HOME={dir}).',
  'codex.open.copilotLaunched': 'Copilot CLI started (provider={provider}).',
  'codex.open.failed': 'Could not launch: {message}',

  // Copilot launcher
  'copilot.open.title': 'Open Copilot CLI',
  'copilot.open.cli': 'Open Copilot CLI in terminal',
  'copilot.open.cli.help': 'Launches the Copilot CLI with COPILOT_PROVIDER_* environment variables injected.',

  // Misc
  'common.refresh': 'Refresh',
  'common.cancel': 'Cancel',
  'common.save': 'Save',
  'common.delete': 'Delete',
  'common.edit': 'Edit',
  'common.loading': 'Loading…',

  // Header controls
  'header.theme.toggle': 'Toggle theme',
  'header.theme.light': 'Light',
  'header.theme.dark': 'Dark',
  'header.lang.toggle': 'Language',
} as const;

export type MessageKey = keyof typeof en;
