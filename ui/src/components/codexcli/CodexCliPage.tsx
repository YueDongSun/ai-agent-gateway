import { useEffect, useState } from 'react';
import {
  codexCliStatus,
  codexOpenCliTerminal,
  settingsSet,
  type CodexCliStatus,
} from '../../lib/api/codexCli';
import { copilotOpenCliTerminal } from '../../lib/api/copilot';

/// Fixed key names (kept in sync with the backend database/dao/settings.rs).
const KEY_CODEX_CLI_HOME = 'codex_cli_home';
const KEY_OUTBOUND_PROXY = 'outbound_proxy';

export function CodexCliPage() {
  const [status, setStatus] = useState<CodexCliStatus | null>(null);
  const [cliHome, setCliHome] = useState('');
  const [proxy, setProxy] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [copilotMsg, setCopilotMsg] = useState<string | null>(null);

  const refresh = async () => {
    try {
      const s = await codexCliStatus();
      setStatus(s);
      setCliHome(s.codexCliHome ?? '');
      setProxy(s.outboundProxy ?? '');
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  useEffect(() => {
    void refresh();
  }, []);

  const flash = (msg: string) => {
    setNotice(msg);
    setTimeout(() => setNotice(null), 4000);
  };

  const handleSave = async () => {
    setBusy('save');
    setError(null);
    try {
      await settingsSet(KEY_CODEX_CLI_HOME, cliHome.trim());
      await settingsSet(KEY_OUTBOUND_PROXY, proxy.trim());
      await refresh();
      flash('Settings saved');
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const handleOpenTerminal = async () => {
    setBusy('terminal');
    setError(null);
    try {
      const r = await codexOpenCliTerminal();
      flash(`Started codex in the terminal (CODEX_HOME=${r.codexHome})`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const handleOpenCopilotTerminal = async () => {
    setBusy('copilot');
    setError(null);
    setCopilotMsg(null);
    try {
      const r = await copilotOpenCliTerminal();
      setCopilotMsg(
        `Started copilot (provider=${r.providerId}); the terminal has been injected with the COPILOT_PROVIDER_* environment variables.`,
      );
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const s = status;

  return (
    <section className="codexcli-page">
      <h2>Codex CLI Isolated Agent</h2>
      <p className="codexcli-desc">
        Gives Codex CLI its own dedicated configuration directory (an independent CODEX_HOME),
        fully isolated from the Codex desktop app's shared{' '}
        <code>~/.codex</code>. Switching Codex providers only writes to the CLI directory, so
        the desktop app (login, plugins, MCP) is unaffected.
      </p>

      {notice && <div className="codexcli-notice">{notice}</div>}
      {error && <div className="codexcli-error">{error}</div>}

      {s && (
        <div className="codexcli-status">
          <div className="codexcli-status-row">
            <span className="codexcli-status-key">codex CLI</span>
            <span className="codexcli-status-val">
              {s.codexBinary ?? 'Not found (please confirm codex is installed)'}
            </span>
          </div>
          <div className="codexcli-status-row">
            <span className="codexcli-status-key">Active configuration directory</span>
            <span className="codexcli-status-val mono">{s.configDir}</span>
          </div>
          <div className="codexcli-status-row">
            <span className="codexcli-status-key">Isolation status</span>
            <span className="codexcli-status-val">
              {s.isolationEnabled ? (
                <span className="tag tag-green">Isolated</span>
              ) : (
                <span className="tag tag-gray">Not isolated (shared with the desktop app)</span>
              )}
            </span>
          </div>
          <div className="codexcli-status-row">
            <span className="codexcli-status-key">Current Codex provider</span>
            <span className="codexcli-status-val">
              {s.currentProviderName
                ? `${s.currentProviderName} (${s.currentProviderId})`
                : 'Not switched (takes effect after selecting a Codex provider on the Providers page)'}
            </span>
          </div>
          <div className="codexcli-status-row">
            <span className="codexcli-status-key">Directory initialized</span>
            <span className="codexcli-status-val">
              {s.initialized ? 'Yes' : 'No (you need to log in once in the CLI on first run)'}
            </span>
          </div>
        </div>
      )}

      {s && s.isolationEnabled && !s.initialized && (
        <div className="codexcli-onboarding">
          <h4>First-time setup guide</h4>
          <p>
            The isolated directory <code>{s.configDir}</code> has not been initialized yet.
            Click the “Open Codex CLI terminal with proxy” button below and complete a one-time{' '}
            <code>codex</code> login in the terminal that opens (on first launch it will
            automatically create config.toml and auth.json). After that you can switch Codex
            providers on the Providers page; configuration is written to the isolated
            directory, and the desktop app is unaffected.
          </p>
          <button
            className="btn btn-primary"
            onClick={handleOpenTerminal}
            disabled={busy === 'terminal'}
          >
            {busy === 'terminal' ? 'Starting…' : 'Open Codex CLI terminal to finish setup'}
          </button>
        </div>
      )}

      <div className="codexcli-field">
        <label>
          Codex CLI configuration directory (CODEX_HOME)
          <input
            type="text"
            className="codexcli-input"
            value={cliHome}
            onChange={(e) => setCliHome(e.target.value)}
            placeholder="e.g. C:\Users\you\.codex-cli (blank = share ~/.codex with the desktop app)"
          />
        </label>
        <p className="codexcli-hint">
          Once an isolated directory is set, AAG writes Codex's live configuration
          (config.toml / auth.json) into that directory, and the desktop app's{' '}
          <code>~/.codex</code> is no longer touched.
        </p>
      </div>

      <div className="codexcli-field">
        <label>
          Global outbound proxy (optional, injected into the CLI terminal)
          <input
            type="text"
            className="codexcli-input"
            value={proxy}
            onChange={(e) => setProxy(e.target.value)}
            placeholder="e.g. http://127.0.0.1:7890 (blank = direct connection)"
          />
        </label>
        <p className="codexcli-hint">
          Injects HTTP_PROXY / HTTPS_PROXY / ALL_PROXY when launching the CLI terminal;
          NO_PROXY is set to localhost/127.0.0.1 by default.
        </p>
      </div>

      <div className="codexcli-actions">
        <button className="btn" onClick={handleSave} disabled={busy === 'save'}>
          {busy === 'save' ? 'Saving…' : 'Save settings'}
        </button>
        <button
          className="btn btn-primary"
          onClick={handleOpenTerminal}
          disabled={busy === 'terminal'}
        >
          {busy === 'terminal' ? 'Starting…' : 'Open Codex CLI terminal with proxy'}
        </button>
      </div>

      <div className="codexcli-copilot">
        <h4>Copilot CLI (GitHub's official AI CLI)</h4>
        <p className="codexcli-hint">
          Copilot CLI custom providers can only be injected through environment variables at
          launch time (there is no persistent config file). Add and switch to a Copilot
          provider on the Providers page (settings_config containing{' '}
          <code>env.COPILOT_PROVIDER_BASE_URL</code> / <code>API_KEY</code> /{' '}
          <code>TYPE</code> + <code>model</code>), then click the button below to open a
          terminal with the <code>COPILOT_PROVIDER_*</code> environment variables injected.
          It also writes{' '}
          <code>~/.copilot/settings.json</code> to persist the model selection.
        </p>
        <button
          className="btn btn-primary"
          onClick={handleOpenCopilotTerminal}
          disabled={busy === 'copilot'}
        >
          {busy === 'copilot' ? 'Starting…' : 'Open Copilot CLI terminal with proxy'}
        </button>
        {copilotMsg && <div className="codexcli-notice">{copilotMsg}</div>}
      </div>
    </section>
  );
}
