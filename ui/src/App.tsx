import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { appCheckUpdate } from './lib/api/app';
import { switchProvider } from './lib/api/providers';
import type { DeeplinkAction } from './lib/api/deeplink';
import { ProvidersPage } from './components/providers/ProvidersPage';
import { ProxyPage } from './components/proxy/ProxyPage';
import { BackupPage } from './components/backup/BackupPage';
import { McpPage } from './components/mcp/McpPage';
import { SkillsPage } from './components/skills/SkillsPage';
import { SessionsPage } from './components/sessions/SessionsPage';
import { UsagePage } from './components/usage/UsagePage';
import { SyncPage } from './components/sync/SyncPage';
import { DeepLinkPage } from './components/deeplink/DeepLinkPage';
import { CodexCliPage } from './components/codexcli/CodexCliPage';

interface AppInfo {
  name: string;
  version: string;
  phase: string;
}

type View = 'app' | 'providers' | 'proxy' | 'backup' | 'mcp' | 'skills' | 'sessions' | 'usage' | 'sync' | 'deeplink' | 'codexcli';

export function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [view, setView] = useState<View>('providers');
  const [updateMsg, setUpdateMsg] = useState<string | null>(null);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [updateEndpoint, setUpdateEndpoint] = useState('');

  useEffect(() => {
    invoke<AppInfo>('app_info')
      .then(setInfo)
      .catch((e) => setError(String(e)));
  }, []);

  const handleCheckUpdate = async () => {
    setCheckingUpdate(true);
    setUpdateMsg(null);
    try {
      const latest = await appCheckUpdate({
        endpoint: updateEndpoint.trim() || null,
      });
      if (latest) {
        setUpdateMsg(`Update available: v${latest}`);
      } else {
        setUpdateMsg('No update available.');
      }
    } catch (e) {
      setUpdateMsg(`Update check failed: ${String(e)}`);
    } finally {
      setCheckingUpdate(false);
    }
  };

  /// Dispatch a parsed deep-link action. Returns a human-readable result
  /// string for the DeepLinkPage to render. Errors are caught here so the
  /// page can show them without crashing.
  const handleExecuteDeeplink = async (
    action: DeeplinkAction,
  ): Promise<string> => {
    switch (action.kind) {
      case 'switchProvider': {
        const r = await switchProvider(action.providerId, action.appType);
        return `Switched to ${action.providerId} (wrote ${r.written.length} file(s))`;
      }
      case 'focusProvider':
        setView('providers');
        return `Opened Providers page (focus: ${action.providerId})`;
      case 'focusMcp':
        setView('mcp');
        return `Opened MCP page (focus: ${action.serverId})`;
      case 'focusSkill':
        setView('skills');
        return `Opened Skills page (focus: ${action.skillId})`;
      case 'showDeeplinks':
        setView('deeplink');
        return 'Opened Deep links page';
      case 'unknown':
        return `Unknown action: ${action.reason}`;
    }
  };

  if (error) {
    return (
      <div className="aag-root">
        <pre className="aag-error">Error: {error}</pre>
      </div>
    );
  }

  return (
    <div className="aag-root">
      <header className="aag-header">
        <div className="aag-header-row">
          <h1>{info?.name ?? 'AI Agent Gateway'}</h1>
          <nav className="aag-nav">
            <button
              className={view === 'providers' ? 'nav-item active' : 'nav-item'}
              onClick={() => setView('providers')}
            >
              Providers
            </button>
            <button
              className={view === 'proxy' ? 'nav-item active' : 'nav-item'}
              onClick={() => setView('proxy')}
            >
              Proxy
            </button>
            <button
              className={view === 'backup' ? 'nav-item active' : 'nav-item'}
              onClick={() => setView('backup')}
            >
              Backup
            </button>
            <button
              className={view === 'mcp' ? 'nav-item active' : 'nav-item'}
              onClick={() => setView('mcp')}
            >
              MCP
            </button>
            <button
              className={view === 'skills' ? 'nav-item active' : 'nav-item'}
              onClick={() => setView('skills')}
            >
              Skills
            </button>
            <button
              className={view === 'sessions' ? 'nav-item active' : 'nav-item'}
              onClick={() => setView('sessions')}
            >
              Sessions
            </button>
            <button
              className={view === 'usage' ? 'nav-item active' : 'nav-item'}
              onClick={() => setView('usage')}
            >
              Usage
            </button>
            <button
              className={view === 'sync' ? 'nav-item active' : 'nav-item'}
              onClick={() => setView('sync')}
            >
              Sync
            </button>
            <button
              className={view === 'deeplink' ? 'nav-item active' : 'nav-item'}
              onClick={() => setView('deeplink')}
            >
              Deep links
            </button>
            <button
              className={view === 'codexcli' ? 'nav-item active' : 'nav-item'}
              onClick={() => setView('codexcli')}
            >
              Codex CLI
            </button>
            <button
              className={view === 'app' ? 'nav-item active' : 'nav-item'}
              onClick={() => setView('app')}
            >
              About
            </button>
          </nav>
        </div>
        {info && (
          <p className="aag-sub">
            v{info.version} · Phase {info.phase}
          </p>
        )}
      </header>
      <main className="aag-main">
        {view === 'providers' ? (
          <ProvidersPage />
        ) : view === 'proxy' ? (
          <ProxyPage />
        ) : view === 'backup' ? (
          <BackupPage />
        ) : view === 'mcp' ? (
          <McpPage />
        ) : view === 'skills' ? (
          <SkillsPage />
        ) : view === 'sessions' ? (
          <SessionsPage />
        ) : view === 'usage' ? (
          <UsagePage />
        ) : view === 'sync' ? (
          <SyncPage />
        ) : view === 'deeplink' ? (
          <DeepLinkPage onExecute={handleExecuteDeeplink} />
        ) : view === 'codexcli' ? (
          <CodexCliPage />
        ) : (
          <section>
            <h2>About</h2>
            <p>
              AAG (AI Agent Gateway) is a Tauri desktop app for managing provider
              configuration across multiple AI coding CLIs (Claude Code, Codex,
              Gemini CLI, OpenCode, OpenClaw, Hermes, Grok Build, Claude Desktop).
            </p>
            <p>
              See <code>DESIGN.md</code> for the migration plan and{' '}
              <code>PROGRESS.md</code> for current status.
            </p>
            <div className="about-update">
              <input
                type="text"
                className="about-update-endpoint"
                value={updateEndpoint}
                onChange={(e) => setUpdateEndpoint(e.target.value)}
                placeholder="https://releases.example.com/aag/latest.json (optional)"
                disabled={checkingUpdate}
              />
              <button
                className="btn"
                onClick={handleCheckUpdate}
                disabled={checkingUpdate}
              >
                {checkingUpdate ? 'Checking…' : 'Check for update'}
              </button>
              {updateMsg && (
                <span className="about-update-msg">{updateMsg}</span>
              )}
            </div>
          </section>
        )}
      </main>
      <footer className="aag-footer">
        Local proxy with circuit breaker + failover (streaming SSE, OpenAI &
        Anthropic compatible) · SQLite SSOT · backup · MCP · skills · sessions ·
        usage · S3/WebDAV sync · deep links · tray.
      </footer>
    </div>
  );
}