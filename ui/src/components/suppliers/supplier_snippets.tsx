/// Four-protocol client connection snippets for the single AAG listen URL.
/// Display and copy only — this page never edits client files.
import { useState } from 'react';
import { useT } from '../../lib/ctx.js';
import { client_snippets } from '../../lib/api/suppliers.js';
import './suppliers_page.css';

export interface Supplier_snippets_props {
  listen_url: string | null;
}

export function Supplier_snippets(props: Supplier_snippets_props) {
  const t = useT();
  const [copied, set_copied] = useState<string | null>(null);
  const snippets = props.listen_url ? client_snippets(props.listen_url) : [];
  async function copy(protocol: string, body: string) {
    try {
      await navigator.clipboard.writeText(body);
      set_copied(protocol);
    } catch {
      set_copied(null);
    }
  }
  return (
    <section className="supplier_panel" aria-label={t('suppliers.snippets.title')} data-testid="suppliers_snippets">
      <h3>{t('suppliers.snippets.title')}</h3>
      <p className="supplier_hint">{t('suppliers.snippets.hint')}</p>
      {!props.listen_url ? (
        <p className="supplier_empty_note">{t('suppliers.status.not_running')}</p>
      ) : (
        <div className="supplier_snippet_grid">
          {snippets.map((snippet) => (
            <article key={snippet.protocol} className="supplier_snippet" data-testid={`suppliers_snippet_${snippet.protocol}`}>
              <header>
                <strong>{t(`suppliers.protocol.${snippet.protocol}`)}</strong>
                <code>{snippet.path}</code>
              </header>
              <pre>{snippet.body}</pre>
              <button type="button" className="btn small" onClick={() => void copy(snippet.protocol, snippet.body)} data-testid={`suppliers_snippet_copy_${snippet.protocol}`}>
                {copied === snippet.protocol ? t('suppliers.snippets.copied') : t('suppliers.snippets.copy')}
              </button>
            </article>
          ))}
        </div>
      )}
      <p className="supplier_hint">{t('suppliers.snippets.token_note')}</p>
    </section>
  );
}
