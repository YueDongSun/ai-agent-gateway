import { useCallback, useEffect, useRef, useState } from 'react';
import { useCtx } from '../../lib/ctx.js';
import {
  cancel_tailnet_discovery, get_tailnet_discovery_status, instance_endpoint,
  scan_tailnet_instances, tailnet_discovery_error, type Aag_instance, type Discovery_report,
} from '../../lib/api/tailnet_discovery.js';
import './tailnet_instances.css';

const discovery_interval_ms = 60_000;
const copy = {
  en: {
    title: 'AAG on your Tailnet', ready: 'Ready to scan', scanning: 'Scanning', cancelling: 'Cancelling',
    complete: 'Scan complete', cancelled: 'Scan cancelled', error: 'Scan unavailable', scan: 'Scan now', cancel: 'Cancel scan',
    auto: 'Check every minute while this panel is open', note: 'Checks the saved gateway port on verified Tailnet devices. No credentials are sent.',
    empty: 'No verified AAG instance answered on this port.', online: 'Observed online', self: 'This device',
    limited: 'This scan reached the 64-device limit. Remaining devices were skipped.', port: 'Port', checked: 'Checked',
    auth: 'Auth protected', offline: 'Not responding', unverified: 'Other / unverified', failed: 'Probe errors', skipped: 'Skipped',
    copy: 'Copy endpoint', copied: 'Copied', copy_failed: 'Copy failed', last: 'Last scan', demo: 'Synthetic demo results',
    unknown: 'Waiting for saved port',
    stopped: 'Tailscale is not running. Start Tailscale and scan again.',
    platform: 'Native Tailnet discovery is currently supported on Windows.',
    unavailable: 'The installed Tailscale client is unavailable. Check the local installation.',
    invalid: 'Tailnet status could not be verified. No unverified destination was probed.',
    timeout: 'The scan exceeded its time budget. Try again.', busy: 'Another discovery scan is still finishing. Try again shortly.',
    generic: 'Discovery could not complete. Scan again after checking Tailscale and the saved port.',
  },
  zh: {
    title: 'Tailnet 中的 AAG', ready: '等待扫描', scanning: '正在扫描', cancelling: '正在取消',
    complete: '扫描完成', cancelled: '扫描已取消', error: '扫描不可用', scan: '立即扫描', cancel: '取消扫描',
    auto: '此页面打开时每分钟检查', note: '检查已验证 Tailnet 设备上的已保存网关端口，不发送凭据。',
    empty: '此端口上没有已验证的 AAG 实例响应。', online: '扫描时在线', self: '此设备',
    limited: '已达到 64 台设备的扫描上限，其余设备已跳过。', port: '端口', checked: '已检查',
    auth: '需要认证', offline: '未响应', unverified: '其他或未验证', failed: '探测错误', skipped: '已跳过',
    copy: '复制端点', copied: '已复制', copy_failed: '复制失败', last: '上次扫描', demo: '模拟演示结果',
    unknown: '等待读取端口',
    stopped: 'Tailscale 未运行，请启动后重试。', platform: '原生 Tailnet 发现目前支持 Windows。',
    unavailable: '已安装的 Tailscale 客户端不可用，请检查本地安装。', invalid: '无法验证 Tailnet 状态，未探测未验证地址。',
    timeout: '扫描超时，请重试。', busy: '另一个发现扫描尚未结束，请稍后重试。', generic: '发现未完成，请检查 Tailscale 和已保存端口后重试。',
  },
} as const;

function error_message(code: string, text: typeof copy['en'] | typeof copy['zh']): string {
  if (code === 'tailnet_discovery_stopped') return text.stopped;
  if (code === 'tailnet_discovery_platform_unsupported') return text.platform;
  if (code === 'tailnet_discovery_process_unavailable') return text.unavailable;
  if (code.includes('timeout') || code === 'tailnet_discovery_timed_out') return text.timeout;
  if (code === 'tailnet_discovery_busy') return text.busy;
  if (code.includes('invalid') || code.includes('too_large') || code === 'tailnet_discovery_peer_limit') return text.invalid;
  return text.generic;
}

export function Tailnet_instances({ on_state }: { on_state?: (state: string) => void }) {
  const { locale } = useCtx();
  const text = copy[locale];
  const [phase, set_phase] = useState<'ready' | 'scanning' | 'cancelling' | 'complete' | 'cancelled' | 'error'>('ready');
  const [report, set_report] = useState<Discovery_report | null>(null);
  const [port, set_port] = useState<number | null>(null);
  const [error, set_error] = useState<string | null>(null);
  const [automatic, set_automatic] = useState(false);
  const [copy_state, set_copy_state] = useState<{ address: string; state: 'copied' | 'copy_failed' } | null>(null);
  const active_request = useRef<string | null>(null);
  const mounted = useRef(false);
  const sequence = useRef(0);
  const copy_sequence = useRef(0);

  const scan = useCallback(async () => {
    if (!mounted.current || active_request.current) return;
    const nonce = globalThis.crypto?.randomUUID?.() ?? `${Date.now()}_${++sequence.current}_${Math.random().toString(36).slice(2)}`;
    const request_id = `tailnet_${nonce}`;
    active_request.current = request_id;
    copy_sequence.current += 1;
    set_phase('scanning'); set_error(null); set_report(null); set_copy_state(null);
    try {
      const result = await scan_tailnet_instances(request_id);
      if (mounted.current && active_request.current === request_id) {
        set_report(result); set_port(result.port); set_phase('complete');
      }
    } catch (cause) {
      const code = tailnet_discovery_error(cause);
      if (mounted.current && active_request.current === request_id) {
        set_phase(code === 'tailnet_discovery_cancelled' ? 'cancelled' : 'error');
        set_error(code === 'tailnet_discovery_cancelled' ? null : code);
      }
    } finally { if (active_request.current === request_id) active_request.current = null; }
  }, []);

  useEffect(() => { on_state?.(phase === 'complete' ? `${report?.instances.length ?? 0} online` : text[phase]); }, [phase, report, text, on_state]);
  useEffect(() => () => { on_state?.('Paused'); }, [on_state]);

  useEffect(() => {
    mounted.current = true;
    let alive = true;
    get_tailnet_discovery_status().then((status) => {
      if (!alive || active_request.current) return;
      set_port(status.port);
      const matching_report = status.lastReport?.port === status.port ? status.lastReport : null;
      set_report(status.error || status.phase === 'scanning' || status.phase === 'cancelling' ? null : matching_report);
      if (status.phase === 'scanning' || status.phase === 'cancelling') { set_error('tailnet_discovery_busy'); set_phase('error'); }
      else if (status.error) { set_error(status.phase === 'cancelled' ? null : tailnet_discovery_error(status.error)); set_phase(status.phase === 'cancelled' ? 'cancelled' : 'error'); }
      else if (matching_report) set_phase('complete');
    }).catch((cause) => { if (alive && !active_request.current) { set_error(tailnet_discovery_error(cause)); set_phase('error'); } });
  return () => {
      alive = false; mounted.current = false;
      const request_id = active_request.current;
      active_request.current = null;
      if (request_id) void cancel_tailnet_discovery(request_id).catch(() => undefined);
    };
  }, []);

  useEffect(() => {
    if (!automatic) return;
    void scan();
    const timer = window.setInterval(() => { void scan(); }, discovery_interval_ms);
    return () => window.clearInterval(timer);
  }, [automatic, scan]);

  const cancel = async () => {
    const request_id = active_request.current;
    if (!request_id) return;
    set_phase('cancelling');
    try { await cancel_tailnet_discovery(request_id); }
    catch (cause) { if (mounted.current && active_request.current === request_id) { set_error(tailnet_discovery_error(cause)); set_phase('scanning'); } }
  };
  const busy = phase === 'scanning' || phase === 'cancelling';
  const counts = report?.counts;
  const copy_instance = async (instance: Aag_instance) => {
    const request = ++copy_sequence.current;
    try {
      await navigator.clipboard.writeText(instance_endpoint(instance));
      if (mounted.current && request === copy_sequence.current) set_copy_state({ address: instance.address, state: 'copied' });
    } catch {
      if (mounted.current && request === copy_sequence.current) set_copy_state({ address: instance.address, state: 'copy_failed' });
    }
  };
  return (
    <section className="card tailnet_instances" data-testid="tailnet-instances" aria-labelledby="tailnet_instances_title">
      <div className="card-head">
        <div><h3 id="tailnet_instances_title">{text.title}</h3><p className="muted small">{port ? `${text.port} ${port}` : text.unknown}</p></div>
        <div className="header-actions">
          <span className={`tailnet_discovery_state ${phase}`} role="status" aria-live="polite" data-testid="tailnet-discovery-state">{text[phase]}</span>
          {busy ? <button className="btn small" data-testid="tailnet-discovery-cancel" onClick={() => void cancel()} disabled={phase === 'cancelling'}>{text.cancel}</button>
            : <button className="btn small" data-testid="tailnet-discovery-scan" onClick={() => void scan()}>{text.scan}</button>}
        </div>
      </div>
      <p className="muted small">{text.note}</p>
      <label className="tailnet_auto"><input type="checkbox" checked={automatic} onChange={(event) => set_automatic(event.target.checked)} data-testid="tailnet-discovery-auto" />{text.auto}</label>
      {error && <p className="error-banner" role="alert" data-testid="tailnet-discovery-error">{error_message(error, text)}</p>}
      {report && <>
        <p className="muted small" data-testid="tailnet-discovery-summary">{text.last}: {new Date(report.checkedAt).toLocaleTimeString(locale === 'zh' ? 'zh-CN' : 'en-US')} · {text.checked} {report.checkedCount}/{report.peerCount} · {report.elapsedMs} ms {report.synthetic ? `· ${text.demo}` : ''}</p>
        <dl className="tailnet_counts">
          {([['authRequired', text.auth], ['offline', text.offline], ['unverified', text.unverified], ['failed', text.failed], ['skipped', text.skipped]] as const).map(([key, label]) => <div key={key}><dt>{label}</dt><dd>{counts?.[key] ?? 0}</dd></div>)}
        </dl>
        {report.limited && <p className="notice-banner">{text.limited}</p>}
        {report.instances.length === 0 ? <p className="muted" data-testid="tailnet-discovery-empty">{text.empty}</p>
          : <ul className="tailnet_instance_list" data-testid="tailnet-instance-list">{report.instances.map((instance) => <li key={instance.address}>
            <div><strong>{instance.deviceName}</strong>{instance.isSelf && <span className="tag">{text.self}</span>}<code>{instance_endpoint(instance).slice('http://'.length)}</code></div>
            <div className="tailnet_instance_status"><span className="tailnet_online">{text.online}</span><span className="muted small">v{instance.version} · {instance.latencyMs} ms</span>
              <button className="btn small" onClick={() => void copy_instance(instance)}>{copy_state?.address === instance.address ? text[copy_state.state] : text.copy}</button>
            </div>
          </li>)}</ul>}
      </>}
    </section>
  );
}
