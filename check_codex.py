import sqlite3, json

db = sqlite3.connect(r'C:\Users\15063\.cc-switch\cc-switch.db')
db.row_factory = sqlite3.Row
cur = db.cursor()

# Get full Codex provider configs
cur.execute("SELECT id, name, app_type, is_current, settings_config FROM providers WHERE app_type='codex'")
rows = cur.fetchall()
print("=== Codex Providers ===")
for r in rows:
    cfg = json.loads(r['settings_config']) if r['settings_config'] else {}
    print(f"\n[{r['id']}] {r['name']} (current={r['is_current']})")
    # Print base_url and api_key info
    if isinstance(cfg, dict):
        base_url = cfg.get('base_url') or cfg.get('baseURL') or '(none)'
        api_key = cfg.get('api_key') or cfg.get('apiKey') or '(none)'
        model = cfg.get('model') or cfg.get('defaultModel') or '(none)'
        print(f"  base_url: {base_url}")
        print(f"  api_key: {api_key[:20]}..." if len(str(api_key)) > 20 else f"  api_key: {api_key}")
        print(f"  model: {model}")
        print(f"  full keys: {list(cfg.keys())}")

# Also check Claude provider for reference
cur.execute("SELECT id, name, settings_config FROM providers WHERE app_type='claude' AND is_current=1")
r = cur.fetchone()
if r:
    cfg = json.loads(r['settings_config']) if r['settings_config'] else {}
    print(f"\n=== Current Claude Provider: [{r['id']}] {r['name']} ===")
    env = cfg.get('env', {})
    print(f"  ANTHROPIC_BASE_URL: {env.get('ANTHROPIC_BASE_URL', '(none)')}")
    print(f"  ANTHROPIC_AUTH_TOKEN: {str(env.get('ANTHROPIC_AUTH_TOKEN', ''))[:20]}...")

db.close()
