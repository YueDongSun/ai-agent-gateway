import type { MessageKey } from '../i18n/en.js';
import type { Error_translator } from './backend_error.js';

const max_error_length = 4096;
const stages: Readonly<Record<string, MessageKey>> = {
  preflight: 'suppliers.discovery.stage.preflight', configuration: 'suppliers.discovery.stage.configuration',
  search: 'suppliers.discovery.stage.search', validation: 'suppliers.discovery.stage.validation',
  catalog: 'suppliers.discovery.stage.catalog', import: 'suppliers.discovery.stage.import',
  admission: 'suppliers.discovery.stage.admission', revalidation: 'suppliers.discovery.stage.revalidation',
};
const reasons: Readonly<Record<string, MessageKey>> = {
  supplier_disabled: 'suppliers.discovery.reason.disabled', auto_tracking_disabled: 'suppliers.discovery.reason.opt_out',
  url_type_unprobeable: 'suppliers.discovery.reason.unsupported', supplier_url_invalid: 'suppliers.discovery.reason.invalid_url',
  no_configured_endpoint: 'suppliers.discovery.reason.no_endpoint', subscription_expired: 'suppliers.discovery.reason.expired',
  subscription_inactive: 'suppliers.discovery.reason.inactive', subscription_unavailable: 'suppliers.discovery.reason.subscription_unknown',
  configuration_obsolete: 'suppliers.discovery.reason.changed', source_changed: 'suppliers.discovery.reason.changed',
  provider_removed: 'suppliers.discovery.reason.changed', provider_subscription_source_changed: 'suppliers.discovery.reason.changed',
  AAG_MODEL_DISCOVERY_PROVIDER_CONFLICT: 'suppliers.discovery.reason.changed', supplier_config_generation_conflict: 'suppliers.discovery.reason.changed',
  screening_unavailable: 'suppliers.discovery.reason.screening', provider_admission_excluded: 'suppliers.discovery.reason.screening',
  provider_subscription_missing: 'suppliers.discovery.reason.screening', subscription_source_not_found: 'suppliers.discovery.reason.source_missing',
  url_type_already_configured: 'suppliers.discovery.reason.existing', url_already_configured: 'suppliers.discovery.reason.existing',
  no_advertised_url_types: 'suppliers.discovery.reason.no_types', supplier_url_discovery_unsupported: 'suppliers.discovery.reason.unsupported',
  openapi_unavailable: 'suppliers.discovery.reason.metadata_missing', openapi_document_invalid: 'suppliers.discovery.reason.metadata_invalid',
  openapi_paths_missing: 'suppliers.discovery.reason.metadata_invalid', openapi_size_limit: 'suppliers.discovery.reason.limit',
  openapi_paths_limit: 'suppliers.discovery.reason.limit', openapi_candidates_limit: 'suppliers.discovery.reason.limit',
  url_search_timeout: 'suppliers.discovery.reason.timeout', url_request_timeout: 'suppliers.discovery.reason.timeout',
  proxy_models_timeout: 'suppliers.discovery.reason.timeout', supplier_models_timeout: 'suppliers.discovery.reason.timeout',
  supplier_discovery_timeout: 'suppliers.discovery.reason.timeout', url_transport_failed: 'suppliers.discovery.reason.connection',
  proxy_models_transport_failed: 'suppliers.discovery.reason.connection', supplier_models_transport_failed: 'suppliers.discovery.reason.connection',
  url_validation_route_unproven: 'suppliers.discovery.reason.route_unproven', url_validation_runtime_path_mismatch: 'suppliers.discovery.reason.route_unproven',
  url_validation_model_path_invalid: 'suppliers.discovery.reason.model_invalid', url_validation_empty_catalog: 'suppliers.discovery.reason.empty_catalog',
  proxy_models_decode_failed: 'suppliers.discovery.reason.catalog_invalid', proxy_models_pagination_invalid: 'suppliers.discovery.reason.catalog_invalid',
  supplier_models_decode_failed: 'suppliers.discovery.reason.catalog_invalid', supplier_models_pagination_invalid: 'suppliers.discovery.reason.catalog_invalid',
  proxy_models_limit_exceeded: 'suppliers.discovery.reason.limit', supplier_models_limit_exceeded: 'suppliers.discovery.reason.limit',
  database_error: 'error.database', io_error: 'error.io', json_error: 'error.json', operation_failed: 'error.operation_failed',
  supplier_error_unknown: 'suppliers.discovery.reason.unknown',
  supplier_subscription_screening_required: 'suppliers.discovery.reason.screening',
  supplier_endpoint_url_invalid: 'suppliers.discovery.reason.invalid_url', supplier_endpoint_url_required: 'suppliers.discovery.reason.invalid_url',
  supplier_endpoint_self_loop: 'suppliers.discovery.reason.self_loop',
  url_http_client_failed: 'suppliers.discovery.reason.client_setup', proxy_http_client_build_failed: 'suppliers.discovery.reason.client_setup',
  supplier_http_client_build_failed: 'suppliers.discovery.reason.client_setup',
};
const protocols: Readonly<Record<string, MessageKey>> = {
  anthropic: 'suppliers.protocol.anthropic', 'openai-chat': 'suppliers.protocol.openai-chat',
  'openai-responses': 'suppliers.protocol.openai-responses', gemini: 'suppliers.protocol.gemini',
};

function reason_text(code: string, t: Error_translator): string {
  const reason = code.replace(/^document_[0-9]+:/, '').replace(/^url_unavailable:/, '');
  const primary = reason.split(':', 1)[0];
  if (Object.hasOwn(reasons, primary)) return t(reasons[primary]);
  const http = /^(proxy_models_http_error_|supplier_models_http_error_|openapi_http_|url_validation_http_)([1-5][0-9]{2})$/.exec(reason);
  if (http) {
    const status = http[2];
    if (http[1] === 'openapi_http_' && ['404', '405'].includes(status)) return t('suppliers.discovery.reason.metadata_missing');
    if (['401', '403'].includes(status)) return t('suppliers.discovery.reason.authentication', { status });
    if (status === '429') return t('suppliers.discovery.reason.rate_limited');
    return t('suppliers.discovery.reason.http', { status });
  }
  return t('suppliers.discovery.reason.unknown');
}

function explain_entry(raw: string, t: Error_translator): string {
  const url = /^supplier_url_(skipped|failed):([^:]+):([^:]+):(.*)$/.exec(raw);
  const supplier = /^supplier_import_(excluded|skipped|failed):(.*)$/.exec(raw);
  const context: string[] = [];
  let reason = raw;
  if (url) {
    context.push(t(`suppliers.discovery.outcome.${url[1]}`));
    context.push(t(Object.hasOwn(protocols, url[2]) ? protocols[url[2]] : 'suppliers.discovery.protocol_unknown'));
    context.push(t(Object.hasOwn(stages, url[3]) ? stages[url[3]] : 'suppliers.discovery.stage.unknown'));
    reason = url[4];
  } else if (supplier) {
    context.push(t(`suppliers.discovery.outcome.${supplier[1]}`));
    reason = supplier[2];
    if (supplier[1] === 'failed') {
      const separator = reason.indexOf(':');
      const stage = reason.slice(0, separator);
      if (separator > 0 && Object.hasOwn(stages, stage)) {
        context.push(t(stages[stage]));
        reason = reason.slice(separator + 1);
      }
    }
  }
  const explanation = reason_text(reason, t);
  return context.length ? t('suppliers.discovery.result_reason', { context: context.join(' · '), reason: explanation }) : explanation;
}

/** Translate only discovery codes; other existing UI/backend errors keep their owner. */
export function supplier_discovery_error_text(raw: string, t: Error_translator): string | null {
  const recognized = /^(supplier_import_(?:excluded|skipped|failed):|supplier_url_|supplier_discovery_|proxy_models_|supplier_models_|provider_admission_|provider_subscription_|subscription_source_)/.test(raw);
  if (!recognized && !Object.hasOwn(reasons, raw)) return null;
  if (raw.length > max_error_length) return t('suppliers.discovery.incomplete_notice');
  if (raw.startsWith('supplier_discovery_partial:')) {
    const reason = raw.slice('supplier_discovery_partial:'.length).split(',').map((code) => explain_entry(code, t)).join('; ');
    return t('suppliers.discovery.partial', { reason });
  }
  return explain_entry(raw, t);
}
