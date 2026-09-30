import type { Dsh_upstream } from '../../lib/api/dsh.js';
import { use_onboarding_i18n } from './dsh_onboarding_i18n.js';

export function Dsh_protocol_hint({ upstream }: { upstream: Dsh_upstream }) {
  const t = use_onboarding_i18n();
  const protocol_label = upstream.protocol === 'anthropic-messages' ? t('protocol_anthropic')
    : upstream.protocol === 'openai-completions' ? t('protocol_openai')
      : upstream.protocol === 'openai-responses' ? t('protocol_responses') : t('protocol_unknown');
  return <div className="dsh_protocol_hint" data-testid="dsh_protocol_hint">
    <div><strong>{t('protocol')}</strong><span className="dsh_badge">{protocol_label}</span>
      <span>{t(upstream.source_kind === 'claude_native' ? 'source_native' : upstream.source_kind === 'gateway' ? 'source_gateway' : 'source_pool')}</span></div>
    <p>{upstream.protocol === 'anthropic-messages' ? t('anthropic_hint') : upstream.protocol === 'openai-responses' ? t('responses_hint') : t('openai_hint')}</p>
    {upstream.source_kind === 'gateway' && <p>{t('transport_hint')}</p>}
    {!upstream.available && <p className="dsh_error">{t('unavailable')}: {upstream.reason || upstream.protocol}</p>}
  </div>;
}
