import { useEffect, useState } from 'react';
import {
  deleteSkill,
  listSkills,
  saveSkill,
  type InstalledSkill,
} from '../../lib/api/skills';

interface AddFormState {
  id: string;
  name: string;
  directory: string;
  description: string;
  repoOwner: string;
  repoName: string;
  repoBranch: string;
}

const EMPTY_FORM: AddFormState = {
  id: '',
  name: '',
  directory: '',
  description: '',
  repoOwner: '',
  repoName: '',
  repoBranch: 'main',
};

export function SkillsPage() {
  const [skills, setSkills] = useState<InstalledSkill[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [showAdd, setShowAdd] = useState(false);
  const [form, setForm] = useState<AddFormState>(EMPTY_FORM);

  const refresh = async () => {
    try {
      const list = await listSkills();
      setSkills(list);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  useEffect(() => {
    void refresh();
  }, []);

  const handleDelete = async (id: string) => {
    if (!confirm(`Delete skill "${id}"?`)) return;
    setBusy(true);
    setError(null);
    try {
      await deleteSkill(id);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleAdd = async () => {
    setError(null);
    try {
      const now = Math.floor(Date.now() / 1000);
      const s: InstalledSkill = {
        id: form.id,
        name: form.name,
        description: form.description || null,
        directory: form.directory,
        repoOwner: form.repoOwner || null,
        repoName: form.repoName || null,
        repoBranch: form.repoBranch || 'main',
        contentHash: null,
        enabledClaude: true,
        enabledCodex: true,
        installedAt: now,
        updatedAt: now,
      };
      await saveSkill(s);
      setShowAdd(false);
      setForm(EMPTY_FORM);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const formatDate = (epoch: number): string => {
    try {
      return new Date(epoch * 1000).toLocaleString();
    } catch {
      return String(epoch);
    }
  };

  return (
    <div className="skills-page">
      <header className="page-header">
        <h2>Skills</h2>
        <button className="btn primary" onClick={() => setShowAdd(true)} disabled={busy}>
          + Add skill
        </button>
      </header>

      {error && <pre className="aag-error">{error}</pre>}

      <section className="list-section">
        {skills.length === 0 ? (
          <p className="muted">
            No skills installed. Click <strong>Add skill</strong> to register one.
            Skills with enabled Claude / Codex flags are projected to those apps'
            skills directories on switch.
          </p>
        ) : (
          <ul className="skill-list">
            {skills.map((s) => (
              <li key={s.id} className="skill-card">
                <div className="skill-main">
                  <div className="skill-name">
                    <strong>{s.name}</strong>
                    <code className="skill-id">{s.id}</code>
                  </div>
                  {s.description && <p className="skill-desc">{s.description}</p>}
                  <code className="skill-path">{s.directory}</code>
                  {s.repoOwner && s.repoName && (
                    <span className="skill-repo muted">
                      {s.repoOwner}/{s.repoName}@{s.repoBranch ?? 'main'}
                    </span>
                  )}
                  <div className="skill-toggles muted">
                    Claude: {s.enabledClaude ? 'on' : 'off'} ·
                    Codex: {s.enabledCodex ? 'on' : 'off'} ·
                    Installed: {formatDate(s.installedAt)}
                  </div>
                </div>
                <div className="skill-actions">
                  <button className="btn danger" onClick={() => handleDelete(s.id)} disabled={busy}>
                    Delete
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>

      {showAdd && (
        <div className="modal-backdrop" onClick={() => setShowAdd(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>Add skill</h3>
            <label>
              <span>ID (unique, no spaces)</span>
              <input
                value={form.id}
                onChange={(e) => setForm({ ...form, id: e.target.value })}
                placeholder="my-skill"
              />
            </label>
            <label>
              <span>Display name</span>
              <input
                value={form.name}
                onChange={(e) => setForm({ ...form, name: e.target.value })}
                placeholder="My Skill"
              />
            </label>
            <label>
              <span>Directory (filesystem path)</span>
              <input
                value={form.directory}
                onChange={(e) => setForm({ ...form, directory: e.target.value })}
                placeholder="~/.aag/skills/my-skill"
              />
            </label>
            <label>
              <span>Description (optional)</span>
              <input
                value={form.description}
                onChange={(e) => setForm({ ...form, description: e.target.value })}
                placeholder="What this skill does"
              />
            </label>
            <div style={{ display: 'grid', gridTemplateColumns: '2fr 1fr 1fr', gap: 8 }}>
              <label>
                <span>Repo owner</span>
                <input
                  value={form.repoOwner}
                  onChange={(e) => setForm({ ...form, repoOwner: e.target.value })}
                  placeholder="anthropics"
                />
              </label>
              <label>
                <span>Repo name</span>
                <input
                  value={form.repoName}
                  onChange={(e) => setForm({ ...form, repoName: e.target.value })}
                  placeholder="skills"
                />
              </label>
              <label>
                <span>Branch</span>
                <input
                  value={form.repoBranch}
                  onChange={(e) => setForm({ ...form, repoBranch: e.target.value })}
                  placeholder="main"
                />
              </label>
            </div>
            <div className="modal-actions">
              <button className="btn" onClick={() => setShowAdd(false)}>
                Cancel
              </button>
              <button
                className="btn primary"
                onClick={handleAdd}
                disabled={!form.id || !form.name || !form.directory}
              >
                Save
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}