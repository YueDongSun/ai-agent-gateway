import { useCallback, useEffect, useRef, useState } from 'react';
import { useI18n } from '../../i18n';
import { use_navigation_block } from '../../hooks/use_navigation_block.js';
import {
  provider_screening_overview, provider_screening_recheck, save_screening_thresholds,
  screening_counts, screening_error, screening_label, type Screening_overview, type Screening_recheck_receipt,
} from '../../lib/api/provider_screening.js';

export function Provider_screening_panel({ refresh_key, on_changed }: { refresh_key: number; on_changed?: () => Promise<void> }) {
  const { locale } = useI18n();
  const local_text = (english: string, chinese: string) => locale === 'zh' ? chinese : english;
  const [overview, set_overview] = useState<Screening_overview | null>(null);
  const [latency, set_latency] = useState('');
  const [deadline, set_deadline] = useState('');
  const [busy, set_busy] = useState<string | null>(null);
  const [error, set_error] = useState<string | null>(null);
  const [notice, set_notice] = useState<string | null>(null);
  const mounted = useRef(false);
  const operation = useRef(0);
  const mutation_running = useRef(false);
  const refresh_pending = useRef(false);
  const policy_dirty = useRef(false);

  const accept = useCallback((result: Screening_overview, reset_policy = true) => {
    set_overview(result);
    if (reset_policy) {
      set_latency(String(result.policy.max_latency_ms));
      set_deadline(String(result.policy.probe_deadline_ms));
    }
  }, []);

  const load = useCallback(async () => {
    if (mutation_running.current) { refresh_pending.current = true; return; }
    const ticket = ++operation.current;
    set_busy('load'); set_error(null);
    try {
      const result = await provider_screening_overview();
      if (mounted.current && operation.current === ticket) accept(result, !policy_dirty.current);
    } catch (cause) { if (mounted.current && operation.current === ticket) set_error(screening_error(cause, locale)); }
    finally { if (mounted.current && operation.current === ticket) set_busy(null); }
  }, [accept, locale]);

  useEffect(() => {
    mounted.current = true;
    void load();
    return () => { mounted.current = false; ++operation.current; };
  }, [load, refresh_key]);

  async function perform(kind: string, run: () => Promise<Screening_overview | Screening_recheck_receipt>) {
    if (busy || mutation_running.current) return;
    mutation_running.current = true;
    const ticket = ++operation.current;
    set_busy(kind); set_error(null); set_notice(null);
    let notify_parent = kind === 'policy';
    try {
      const result = await run();
      if ('checkStarted' in result) notify_parent = result.checkStarted !== false;
      if (!mounted.current || operation.current !== ticket) return;
      if ('checkStarted' in result) {
        if (result.overview) accept(result.overview, !policy_dirty.current);
        const errors = [result.checkError, result.runtimeError, result.overviewError].filter((value): value is string => value !== null);
        if (errors.length) set_error(errors.map((code) => screening_error(code, locale)).join(' '));
        if (result.checkStarted === null) {
          set_notice(local_text('The operation outcome is unknown. Closing this view does not cancel a backend check. Read its current status before sending another check.', '操作结果未知。关闭此视图不会取消后端检查。请先确认当前状态，再发送新的检查。'));
        } else if (result.changes.admissionCommitted) {
          const awaiting_registration = result.overview?.entries.some((entry) => entry.providerId === result.providerId && entry.reason === 'awaiting_registration');
          set_notice(result.checkError
            ? local_text('The selected admission state was saved before the check failed. This check did not change other provider records or global admission policy. The current overview shows effective eligibility.', '检查失败前，所选连接的准入状态已保存。此检查没有修改其他连接记录或全局准入策略。当前概览显示实际准入资格。')
            : awaiting_registration
            ? local_text('The selected check was saved. Use the explicit Import action to register this eligible connection. Global admission policy was not changed.', '所选检查结果已保存。请使用“导入”操作注册这个已通过检查的连接。全局准入策略未改变。')
            : local_text('The selected check was saved. This check did not change other provider records or global admission policy. The current overview shows effective eligibility.', '所选检查结果已保存。此检查没有修改其他连接记录或全局准入策略。当前概览显示实际准入资格。'));
        } else if (result.changes.sourceCommitted) {
          set_notice(local_text('The selected source binding was saved, but the admission check did not commit. Refresh status before retrying. This check did not change other provider records or global admission policy.', '所选来源绑定已保存，但准入检查未提交。请刷新状态后再重试。此检查没有修改其他连接记录或全局准入策略。'));
        }
      } else {
        accept(result);
        set_notice(local_text('Thresholds saved for all screening checks. Previously screened connections require a new check under these thresholds before admission can be claimed. No checks were started automatically.', '所有筛选检查的阈值已保存。此前已筛选的连接需要按新阈值重新检查，才能声明准入资格。系统没有自动发起检查。'));
      }
    } catch (cause) {
      // A failed acknowledgement can follow a committed mutation. Read current
      // state once; never repeat the check automatically.
      notify_parent = true;
      try {
        const result = await provider_screening_overview();
        if (mounted.current && operation.current === ticket) accept(result, !policy_dirty.current);
      } catch {
        if (mounted.current && operation.current === ticket) set_error(screening_error('provider_screening_overview_unavailable', locale));
      }
      if (mounted.current && operation.current === ticket) set_error((previous) => [screening_error(cause, locale), previous].filter(Boolean).join(' '));
    }
    finally {
      if (notify_parent && mounted.current) {
        try { await on_changed?.(); }
        catch { if (mounted.current && operation.current === ticket) set_error((previous) => [previous, local_text('The saved state could not be refreshed elsewhere. Refresh status before retrying.', '无法同步刷新其他视图中的已保存状态。请刷新状态后再重试。')].filter(Boolean).join(' ')); }
      }
      mutation_running.current = false;
      if (mounted.current && operation.current === ticket) set_busy(null);
      if (mounted.current && refresh_pending.current) { refresh_pending.current = false; void load(); }
    }
  }

  const counts = screening_counts(overview?.entries ?? []);
  const dirty = overview !== null && (latency !== String(overview.policy.max_latency_ms) || deadline !== String(overview.policy.probe_deadline_ms));
  policy_dirty.current = dirty;
  const mutation_active = busy !== null && busy !== 'load';
  use_navigation_block(mutation_active || dirty, () => set_notice(mutation_active
    ? local_text('Wait for the selected check or threshold save to finish before changing destination.', '请等待当前检查或阈值保存完成后，再切换页面。')
    : local_text('Save or discard threshold edits before changing destination.', '请保存或放弃阈值修改后，再切换页面。')));
  return <section className="supplier_panel" aria-label={local_text('Provider screening', '连接筛选')} data-testid="provider-screening-panel">
    <div className="supplier_card_heading">
      <h3>{local_text('Provider screening', '连接筛选')}</h3>
      <button type="button" className="btn small" disabled={busy !== null} onClick={() => void load()} data-testid="provider-screening-refresh">{local_text('Refresh status', '刷新状态')}</button>
    </div>
    <p className="supplier_hint">{local_text('Subscription and usage are checked before a minimal', '发送最小检查请求前会先检查订阅和用量：')} <code>ok</code> {local_text('request. Expired, inactive, or unverified subscriptions cannot enter the pool. Recheck provider checks only the selected connection; it does not register other candidates or activate global admission. Refresh status only reads results. A configured offered model and matching CC Switch source are required. Discover models is a separate catalog-only diagnostic and does not verify generation or admission.', '已过期、无效或未验证的订阅不能进入池。“重新检查连接”只检查所选连接，不会注册其他候选连接或启用全局准入。“刷新状态”只读取结果。检查需要一个已配置且实际提供的模型，以及匹配的 CC Switch 来源。“发现模型”是独立的目录诊断，不代表生成能力或准入已验证。')}</p>
    {error && <p role="alert" className="supplier_error">{error}</p>}
    {notice && <p role="status" className="supplier_notice">{notice}</p>}
    {busy === 'load' && <p role="status" className="supplier_hint">{local_text('Loading screening status…', '正在加载筛选状态…')}</p>}
    {overview && <>
      <p className="supplier_row" data-testid="provider-screening-counts">
        <span className="supplier_tag">{overview.enforced ? local_text('Admission enforced', '准入已启用') : local_text('Admission not activated', '准入未启用')}</span>
        <span>{counts.providers} {local_text('admitted providers', '个准入连接')} · {counts.pairs} {local_text('provider/model pairs', '个连接/模型组合')} · {counts.excluded} {local_text('excluded entries', '个排除项')}</span>
      </p>
      <form className="supplier_row" onSubmit={(event) => { event.preventDefault(); void perform('policy', () => save_screening_thresholds(Number(latency), Number(deadline))); }}>
        <p className="supplier_hint">{local_text('Thresholds apply to all connections. Changing them requires existing screened connections to be checked again before admission under the new policy.', '阈值适用于所有连接。修改后，已筛选的连接需要重新检查，才能按新策略获得准入资格。')}</p>
        <label className="supplier_field">{local_text('Maximum response latency (ms)', '最大响应延迟（毫秒）')}
          <input type="number" min={1} max={120000} step={1} required value={latency} disabled={busy !== null} onChange={(event) => set_latency(event.target.value)} data-testid="provider-screening-latency" />
        </label>
        <label className="supplier_field">{local_text('Probe deadline (ms)', '检查截止时间（毫秒）')}
          <input type="number" min={1} max={120000} step={1} required value={deadline} disabled={busy !== null} onChange={(event) => set_deadline(event.target.value)} data-testid="provider-screening-deadline" />
        </label>
        <button type="submit" className="btn small" disabled={busy !== null || !dirty}>{local_text('Save thresholds', '保存阈值')}</button>
        {dirty && <button type="button" className="btn small" disabled={busy !== null} data-testid="provider-screening-discard" onClick={() => {
          set_latency(String(overview.policy.max_latency_ms));
          set_deadline(String(overview.policy.probe_deadline_ms));
          set_notice(local_text('Unsaved threshold edits were discarded. Saved thresholds were not changed.', '未保存的阈值修改已放弃，已保存的阈值未改变。'));
        }}>{local_text('Discard threshold edits', '放弃阈值修改')}</button>}
      </form>
      {overview.entries.length === 0 && <p className="supplier_hint">{local_text('No screening records yet. Screened and excluded import candidates will appear here.', '尚无筛选记录。已检查及已排除的导入候选项会显示在这里。')}</p>}
      <ul className="supplier_import_list">
        {overview.entries.map((entry) => <li key={entry.providerId} data-testid="provider-screening-entry">
          <div className="supplier_card_heading">
            <div><strong>{entry.name}</strong><p className="supplier_hint">{entry.admittedModels.length ? `${entry.admittedModels.length} ${local_text('admitted models', '个准入模型')}` : local_text('No models admitted', '没有准入模型')} · {screening_label(entry.reason, locale)}</p></div>
            <button type="button" className="btn small" disabled={busy !== null} onClick={() => void perform(entry.providerId, () => provider_screening_recheck(entry.providerId))} data-testid="provider-screening-recheck">
              {busy === entry.providerId ? local_text('Checking…', '正在检查…') : local_text('Recheck provider', '重新检查连接')}
            </button>
          </div>
          <p className="supplier_hint">{local_text('Subscription:', '订阅：')} {screening_label(entry.subscriptionState, locale)} · {screening_label(entry.subscriptionReason, locale)} · {local_text('Stage:', '阶段：')} {screening_label(entry.stage, locale)}</p>
          <p className="supplier_hint">{local_text('Tested model:', '已测试模型：')} <code>{entry.probeModel ?? local_text('None — no model was tested', '无，尚未测试任何模型')}</code>{entry.latencyMs !== null && <> · {entry.latencyMs} ms</>}{entry.checkedAt !== null && <> · {local_text('Checked', '检查时间')} {new Date(entry.checkedAt * 1000).toLocaleString()}</>}</p>
          <details className="supplier_details">
            <summary>{local_text('Model inventory and bounded probe response', '模型目录与受限检查响应')}</summary>
            <p className="supplier_hint">{local_text('Only the tested model above was probed. Other admitted models are advertised offers from the same screened connection.', '仅上方标注的已测试模型执行过实际检查。其他准入模型是同一已筛选连接公布的模型。')}</p>
            <p className="supplier_hint">{local_text('Admitted models:', '准入模型：')} {entry.admittedModels.length ? entry.admittedModels.join(', ') : local_text('None', '无')}</p>
            {entry.response === null ? <p className="supplier_hint">{local_text('No response was captured.', '未捕获响应。')}</p> : <pre className="supplier_hint" style={{ whiteSpace: 'pre-wrap', overflowWrap: 'anywhere', maxHeight: 240, overflow: 'auto' }}>{entry.response}</pre>}
            {entry.responseTruncated && <p className="supplier_hint">{local_text('Response display was truncated at the capture limit.', '显示内容已按捕获上限截断。')}</p>}
            {entry.responseRedacted && <p className="supplier_hint">{local_text('Sensitive values were redacted from the captured response.', '捕获响应中的敏感值已脱敏。')}</p>}
          </details>
        </li>)}
      </ul>
    </>}
  </section>;
}
