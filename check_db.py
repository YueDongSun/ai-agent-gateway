import sqlite3, json

db = sqlite3.connect(r'C:\Users\15063\.cc-switch\cc-switch.db')
db.row_factory = sqlite3.Row
cur = db.cursor()

# List tables
cur.execute("SELECT name FROM sqlite_master WHERE type='table'")
tables = [r[0] for r in cur.fetchall()]
print("Tables:", ", ".join(tables))

# Check providers
try:
    cur.execute("SELECT id, name, app_type, is_current FROM providers")
    rows = [dict(r) for r in cur.fetchall()]
    print("\nProviders:")
    for r in rows:
        print(f"  [{r['app_type']}] {r['name']} (id={r['id']}, current={r['is_current']})")
except Exception as e:
    print(f"providers error: {e}")

# Check proxy config
try:
    cur.execute("SELECT * FROM proxy_config")
    rows = [dict(r) for r in cur.fetchall()]
    print("\nProxy config:", json.dumps(rows, indent=2, default=str))
except Exception as e:
    print(f"proxy_config error: {e}")

# Check settings
try:
    cur.execute("SELECT key, value FROM settings WHERE key LIKE '%proxy%' OR key LIKE '%port%'")
    rows = [dict(r) for r in cur.fetchall()]
    print("\nProxy-related settings:", json.dumps(rows, indent=2, default=str))
except Exception as e:
    print(f"settings error: {e}")

db.close()
