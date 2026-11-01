import { invoke } from '@tauri-apps/api/core';
import { settingsSet } from './codexCli.js';

type Screening_locale = 'en' | 'zh';
const SCREENING_ERRORS: Record<string, { en: string; zh: string }> = {
  provider_screening_policy_invalid: { en: 'Use whole milliseconds: latency must be at least 1, no greater than the deadline, and the deadline at most 120,000.', zh: '请输入整数毫秒：延迟至少为 1，不能超过截止时间，截止时间不能超过 120,000。' },
  provider_admission_busy: { en: 'A check is already running for this connection.', zh: '此连接已有检查正在进行，请等待其结束。' },
  provider_subscription_source_changed: { en: 'The source or selected connection changed. Refresh the screening information before trying again.', zh: '来源或所选连接已改变。请刷新筛选信息后再试。' },
  subscription_source_invalid: { en: 'The matching CC Switch source is unavailable or invalid. Correct or refresh that source before checking this connection.', zh: '匹配的 CC Switch 来源不可用或无效。请修正或刷新该来源后再检查此连接。' },
  provider_subscription_source_missing: { en: 'This check requires a CC Switch source with verified subscription evidence. Discover models is a catalog-only diagnostic and does not establish admission.', zh: '此检查需要带有已验证订阅证据的 CC Switch 来源。“发现模型”只检查目录，不代表获得准入资格。' },
  provider_screening_model_unconfigured: { en: 'Configure an offered model before checking this connection.', zh: '请先为此连接配置一个实际提供的模型。' },
  provider_disabled: { en: 'Enable the selected connection before checking it.', zh: '请先启用所选连接。' },
  provider_screening_outcome_unknown: { en: 'The check acknowledgement was lost. Status was refreshed where possible; the backend operation was not cancelled. Confirm its result before retrying.', zh: '检查回执丢失。已尽可能刷新状态，后端操作并未因此取消。请确认结果后再重试。' },
  provider_screening_runtime_reconcile_failed: { en: 'Runtime reconciliation failed. Review the recorded check outcome and use gateway recovery controls to apply the current state; rechecking would send another check.', zh: '运行时状态更新失败。请查看已记录的检查结果，并使用网关恢复操作应用当前状态；重新检查会发送新的检查请求。' },
  provider_screening_overview_unavailable: { en: 'Current screening status could not be read. Existing displayed results may be stale; refresh before retrying.', zh: '无法读取当前筛选状态，现有显示结果可能已过期。请刷新后再重试。' },
  default: { en: 'Screening information could not be updated. Your input is preserved; refresh to see the current result.', zh: '无法更新筛选信息。输入已保留，请刷新查看当前结果。' },
};

export interface Screening_policy {
  max_latency_ms: number;
  probe_deadline_ms: number;
  max_output_tokens: number;
  max_response_chars: number;
}

export interface Screening_entry {
  providerId: string;
  name: string;
  admittedModels: string[];
  subscriptionState: string;
  subscriptionReason: string;
  stage: string;
  reason: string;
  probeModel: string | null;
  checkedAt: number | null;
  latencyMs: number | null;
  response: string | null;
  responseTruncated: boolean;
  responseRedacted: boolean;
}

export interface Screening_overview {
  enforced: boolean;
  policy: Screening_policy;
  entries: Screening_entry[];
}

export interface Screening_recheck_receipt {
  providerId: string;
  /** null means the IPC acknowledgement was lost; cancellation is not implied. */
  checkStarted: boolean | null;
  eligible: boolean | null;
  checkError: string | null;
  changes: {
    policyChanged: boolean | null;
    registrationChanged: boolean | null;
    admissionCommitted: boolean | null;
    sourceCommitted: boolean | null;
  };
  admissionVersion: number | null;
  runtimeReconciled: boolean | null;
  runtimeError: string | null;
  overview: Screening_overview | null;
  overviewError: string | null;
}

export const provider_screening_overview = () => invoke<Screening_overview>('provider_screening_overview');
export async function provider_screening_recheck(provider_id: string): Promise<Screening_recheck_receipt> {
  let receipt: Screening_recheck_receipt;
  try {
    receipt = await invoke<Screening_recheck_receipt>('provider_screening_recheck', { providerId: provider_id });
  } catch {
    // A lost acknowledgement does not authorize repeating an inference check.
    receipt = {
      providerId: provider_id, checkStarted: null, eligible: null,
      checkError: 'provider_screening_outcome_unknown',
      changes: { policyChanged: null, registrationChanged: null, admissionCommitted: null, sourceCommitted: null },
      admissionVersion: null, runtimeReconciled: null, runtimeError: null,
      overview: null, overviewError: null,
    };
  }
  if (receipt.overview === null) {
    try {
      receipt = { ...receipt, overview: await provider_screening_overview(), overviewError: null };
    } catch {
      receipt = { ...receipt, overviewError: 'provider_screening_overview_unavailable' };
    }
  }
  return receipt;
}

export function screening_counts(entries: Screening_entry[]) {
  const providers = new Set<string>();
  const pairs = new Set<string>();
  for (const entry of entries) for (const model of entry.admittedModels) {
    providers.add(entry.providerId);
    pairs.add(JSON.stringify([entry.providerId, model]));
  }
  return { providers: providers.size, pairs: pairs.size, excluded: entries.filter((entry) => !entry.admittedModels.length).length };
}

export function updated_screening_policy(policy: Screening_policy, latency: number, deadline: number): Screening_policy {
  if (!Number.isSafeInteger(latency) || !Number.isSafeInteger(deadline)
    || latency < 1 || latency > deadline || deadline > 120_000) {
    throw new Error('provider_screening_policy_invalid');
  }
  return { ...policy, max_latency_ms: latency, probe_deadline_ms: deadline };
}

export async function save_screening_thresholds(latency: number, deadline: number): Promise<Screening_overview> {
  // Read the latest other limits before saving this pair of editable fields.
  const current = await provider_screening_overview();
  const updated = updated_screening_policy(current.policy, latency, deadline);
  await settingsSet('provider_screening_policy', JSON.stringify(updated));
  return provider_screening_overview();
}

const REASONS: Record<string, string> = {
  accepted: 'Accepted', active: 'Active', inactive: 'Inactive', unknown: 'Unknown', not_required: 'Subscription not required',
  subscription_inactive: 'Subscription is inactive', subscription_unknown: 'Subscription status is unverified', subscription_expired: 'Subscription has expired',
  invalid_evidence: 'Subscription evidence is invalid', evidence_provider_mismatch: 'Evidence belongs to another connection',
  invalid_configuration: 'Configuration is invalid', invalid_model: 'Model identifier is invalid', provider_disabled: 'Provider is disabled',
  provider_construction_failed: 'Provider configuration could not be loaded', probe_unsupported: 'Bounded screening is unsupported',
  client_credential_required: 'A client credential is required', timeout: 'Probe exceeded its deadline', connection_refused: 'Connection was refused',
  connection_failed: 'Connection failed', abnormal_response: 'The response was incomplete or invalid', authentication: 'Authentication failed',
  rate_limited: 'Provider rate limit reached', quota_exceeded: 'Provider quota is exhausted', response_too_large: 'Response exceeded the size limit',
  provider_error: 'Provider returned an error', latency_exceeded: 'Response exceeded the latency threshold',
  subscription: 'Subscription check', configuration: 'Configuration check', probe: 'Availability check', admission: 'Pool admission',
  not_checked: 'Not checked', not_screened: 'Not screened', pending: 'Pending screening', not_admitted: 'Not admitted', awaiting_registration: 'Checked; awaiting explicit import',
  missing_evidence: 'Subscription evidence missing', policy_changed: 'Thresholds changed; recheck required', subscription_evidence_expired_or_changed: 'Subscription evidence expired or changed',
};

const REASONS_ZH: Record<string, string> = {
  accepted: '通过', active: '有效', inactive: '无效', unknown: '未知', not_required: '无需订阅',
  subscription_inactive: '订阅无效', subscription_unknown: '订阅状态尚未验证', subscription_expired: '订阅已过期',
  invalid_evidence: '订阅证据无效', evidence_provider_mismatch: '证据属于其他连接',
  invalid_configuration: '配置无效', invalid_model: '模型标识无效', provider_disabled: '连接已禁用',
  provider_construction_failed: '无法加载连接配置', probe_unsupported: '不支持受限检查', client_credential_required: '需要客户端凭据',
  timeout: '检查超过截止时间', connection_refused: '连接被拒绝', connection_failed: '连接失败',
  abnormal_response: '响应不完整或无效', authentication: '身份验证失败', rate_limited: '达到请求速率限制',
  quota_exceeded: '连接配额已用尽', response_too_large: '响应超过大小限制', provider_error: '上游返回错误', latency_exceeded: '响应超过延迟阈值',
  subscription: '订阅检查', configuration: '配置检查', probe: '可用性检查', admission: '池准入',
  not_checked: '尚未检查', not_screened: '尚未筛选', pending: '等待筛选', not_admitted: '未准入', awaiting_registration: '已检查，等待显式导入',
  missing_evidence: '缺少订阅证据', policy_changed: '阈值已改变，需要重新检查', subscription_evidence_expired_or_changed: '订阅证据已过期或改变',
};

export function screening_label(code: string, locale: Screening_locale = 'en'): string {
  return (locale === 'zh' ? REASONS_ZH[code] : REASONS[code])
    ?? REASONS[code] ?? (/^[a-z][a-z0-9_]{0,95}$/.test(code) ? code.replaceAll('_', ' ') : locale === 'zh' ? '无法识别的状态' : 'Unrecognized status');
}

export function screening_error(error: unknown, locale: Screening_locale = 'en'): string {
  let code = String(error ?? '');
  if (code === 'provider_admission_source_changed' || code === 'provider_admission_configuration_changed') code = 'provider_subscription_source_changed';
  if (code.startsWith('subscription_source_')) code = 'subscription_source_invalid';
  return (SCREENING_ERRORS[code] ?? SCREENING_ERRORS.default)[locale];
}
