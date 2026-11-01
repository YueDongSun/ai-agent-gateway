import { useEffect, useState } from 'react';
import { useCtx } from '../../../lib/ctx';
import {
  get_tailnet_publication_status,
  save_tailnet_publication_config,
  start_tailnet_publication,
  stop_tailnet_publication,
  type TailnetPublicationStatus,
} from '../../../lib/api/tailnet_publication.js';

const copy = {
  en: {
    title: 'Tailnet gateway', enabled: 'Auto-start with the gateway', port: 'Tailnet TCP port',
    save: 'Save', start: 'Start sharing', stop: 'Stop sharing', refresh: 'Refresh',
    running: 'Sharing', stopped: 'Stopped', outcome_unknown: 'Ownership needs attention', loading: 'Loading…',
    help: 'Uses the existing gateway API key. Saving settings stops the current publication; the local gateway keeps running.',
    local_required: 'Start the local gateway before sharing.', endpoint: 'Tailnet endpoint',
    unavailable: 'Endpoint unavailable until publication and Tailscale self status are confirmed.',
    demo: 'Synthetic demo controls. No native Tailscale publication or endpoint is created.',
    invalid_port: 'Enter a TCP port from 1 to 65535.', copy: 'Copy endpoint',
  },
  zh: {
    title: 'Tailnet 网关', enabled: '随本地网关自动启动', port: 'Tailnet TCP 端口',
    save: '保存', start: '开始共享', stop: '停止共享', refresh: '刷新',
    running: '共享中', stopped: '已停止', outcome_unknown: '需要确认所有权', loading: '加载中…',
    help: '使用现有网关 API 密钥。保存设置会停止当前共享，本地网关继续运行。',
    local_required: '请先启动本地网关。', endpoint: 'Tailnet 端点',
    unavailable: '确认共享已运行及 Tailscale 本机状态后才会显示端点。',
    demo: '合成演示控件，不会创建原生 Tailscale 共享或实际端点。',
    invalid_port: '请输入 1 至 65535 的 TCP 端口。', copy: '复制端点',
  },
};

interface TailnetPublicationControlsProps {
  gateway_running: boolean;
  on_copy: (value: string, key: string) => void;
}

export function TailnetPublicationControls({ gateway_running, on_copy }: TailnetPublicationControlsProps) {
  const { locale } = useCtx();
  const text = copy[locale === 'zh' ? 'zh' : 'en'];
  const [status, set_status] = useState<TailnetPublicationStatus | null>(null);
  const [enabled, set_enabled] = useState(false);
  const [port, set_port] = useState('8989');
  const [busy, set_busy] = useState(false);
  const [error, set_error] = useState<string | null>(null);

  async function load_status(update_draft = true): Promise<void> {
    const next = await get_tailnet_publication_status();
    set_status(next);
    if (update_draft) { set_enabled(next.config.enabled); set_port(String(next.config.port)); }
  }

  useEffect(() => {
    let disposed = false;
    set_busy(true);
    void get_tailnet_publication_status().then((next) => {
      if (disposed) return;
      set_status(next); set_enabled(next.config.enabled); set_port(String(next.config.port)); set_error(null);
    }).catch((failure: unknown) => {
      if (!disposed) set_error(String(failure));
    }).finally(() => { if (!disposed) set_busy(false); });
    return () => { disposed = true; };
  }, [gateway_running]);

  async function act(action: 'save' | 'start' | 'stop' | 'refresh'): Promise<void> {
    set_busy(true); set_error(null);
    try {
      if (action === 'save' || action === 'start') {
        const parsed_port = Number(port);
        if (!/^\d+$/.test(port) || !Number.isInteger(parsed_port) || parsed_port < 1 || parsed_port > 65535) {
          throw new Error(text.invalid_port);
        }
        await save_tailnet_publication_config({ enabled, port: parsed_port });
      }
      if (action === 'start') await start_tailnet_publication();
      if (action === 'stop') await stop_tailnet_publication();
      await load_status();
    } catch (failure: unknown) {
      set_error(String(failure));
      // A save can commit before a later native start fails. Refresh the
      // actual state while retaining the visible action failure.
      try { await load_status(false); } catch { /* Preserve the action error. */ }
    } finally { set_busy(false); }
  }

  const phase = status?.publication.state;
  return (
    <section className="tailnet_publication_controls" data-testid="tailnet_publication_controls" aria-busy={busy}>
      <div className="endpoint-row">
        <span className="endpoint-label">{text.title}</span>
        <span className={phase === 'running' ? 'pill on' : 'pill off'} data-testid="tailnet_publication_status">
          {phase ? `${status?.synthetic ? 'Demo · ' : ''}${text[phase]}` : text.loading}
        </span>
        <button type="button" className="btn small" disabled={busy} onClick={() => void act('refresh')}>{text.refresh}</button>
      </div>
      <div className="endpoint-row">
        <label><input type="checkbox" checked={enabled} disabled={busy || !status}
          onChange={(event) => set_enabled(event.target.checked)} data-testid="tailnet_publication_enabled" /> {text.enabled}</label>
        <label>{text.port} <input type="number" min="1" max="65535" step="1" value={port}
          disabled={busy || !status} onChange={(event) => set_port(event.target.value)}
          style={{ width: '7em' }} data-testid="tailnet_publication_port" /></label>
        <button type="button" className="btn small" disabled={busy || !status} onClick={() => void act('save')}
          data-testid="tailnet_publication_save">{text.save}</button>
        <button type="button" className="btn small" disabled={busy || !status || !enabled || !gateway_running || phase === 'running'}
          onClick={() => void act('start')} data-testid="tailnet_publication_start">{text.start}</button>
        <button type="button" className="btn small" disabled={busy || !phase || phase === 'stopped'}
          onClick={() => void act('stop')} data-testid="tailnet_publication_stop">{text.stop}</button>
      </div>
      <p className="muted small">{status?.synthetic ? text.demo : text.help}</p>
      {!gateway_running && <p className="muted small">{text.local_required}</p>}
      {status?.endpoint && status.endpoint_source === 'tailscale_status_self' ? (
        <div className="endpoint-row"><span className="endpoint-label">{text.endpoint}</span>
          <code className="endpoint-value" data-testid="tailnet_publication_endpoint">{status.endpoint}</code>
          <button type="button" className="btn small" onClick={() => on_copy(status.endpoint!, 'tailnet_endpoint')}>{text.copy}</button>
        </div>
      ) : <p className="muted small">{text.unavailable}</p>}
      {(error || status?.publication.error || status?.metadata_error) && (
        <p className="error" role="alert" data-testid="tailnet_publication_error">
          {error || status?.publication.error || status?.metadata_error}
        </p>
      )}
    </section>
  );
}
