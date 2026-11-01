import { useEffect, useState } from 'react';
import { codex_reasoning_models, type Codex_reasoning_model } from '../../lib/api/codexCli.js';

interface Codex_reasoning_props {
  values: Record<string, string>;
  on_change: (values: Record<string, string>) => void;
  on_validity_change?: (valid: boolean) => void;
}

export function Codex_reasoning_selector({ models, values, on_change }: Codex_reasoning_props & { models: Codex_reasoning_model[] }) {
  const default_model = models.find((model) => model.isDefault)?.model || '';
  const selected_model = values.model || default_model;
  const efforts = models.find((model) => model.model === selected_model)?.supportedEfforts ?? [];
  const unsupported_effort = values.model_reasoning_effort && !efforts.includes(values.model_reasoning_effort);
  return <>
    <label>Model
      <select className="input" value={selected_model} data-testid="codex_model" onChange={(event) => on_change({ ...values, model: event.target.value, model_reasoning_effort: '' })}>
        {!models.some((model) => model.model === selected_model) && <option value={selected_model}>{selected_model || 'Select a model'}</option>}
        {models.map((model) => <option key={model.model} value={model.model}>{model.model}</option>)}
      </select>
    </label>
    <label>Reasoning effort
      <select className="input" value={values.model_reasoning_effort || ''} data-testid="codex_reasoning_effort" disabled={efforts.length === 0 && !unsupported_effort} onChange={(event) => on_change({ ...values, model_reasoning_effort: event.target.value })}>
        <option value="">Model default</option>
        {unsupported_effort && <option value={values.model_reasoning_effort}>{values.model_reasoning_effort} (unavailable for this model)</option>}
        {efforts.map((effort) => <option key={effort} value={effort}>{effort}</option>)}
      </select>
      {unsupported_effort && <span role="status">Choose a supported effort or Model default before saving.</span>}
    </label>
  </>;
}

export function Codex_reasoning_fields({ provider_id, ...props }: Codex_reasoning_props & { provider_id?: string }) {
  const [models, set_models] = useState<Codex_reasoning_model[]>([]);
  const [failed, set_failed] = useState(false);
  useEffect(() => {
    const selected_model = props.values.model || models.find((model) => model.isDefault)?.model;
    const selected_effort = props.values.model_reasoning_effort;
    props.on_validity_change?.(!selected_effort || !!models.find((model) => model.model === selected_model)?.supportedEfforts.includes(selected_effort));
  }, [models, props.values.model, props.values.model_reasoning_effort, props.on_validity_change]);
  useEffect(() => {
    const default_model = models.find((model) => model.isDefault)?.model;
    if (!props.values.model && default_model) props.on_change({ ...props.values, model: default_model });
  }, [models, props.values.model, props.on_change]);
  useEffect(() => {
    let disposed = false;
    void codex_reasoning_models(provider_id).then((next) => {
      if (!disposed) { set_models(next); set_failed(false); }
    }).catch(() => { if (!disposed) set_failed(true); });
    return () => { disposed = true; };
  }, [provider_id]);
  return <>
    <Codex_reasoning_selector {...props} models={models} />
    {failed && <p role="status">Model capabilities could not be loaded.</p>}
  </>;
}
