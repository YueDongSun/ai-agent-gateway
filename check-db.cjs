const Database = require('better-sqlite3');
const db = new Database('C:/Users/15063/.cc-switch/cc-switch.db', { readonly: true });

const tables = db.prepare("SELECT name FROM sqlite_master WHERE type='table'").all();
console.log('Tables:', tables.map(t => t.name).join(', '));

try {
  const cols = db.prepare('PRAGMA table_info(providers)').all();
  console.log('Provider columns:', cols.map(c => c.name).join(', '));
  const providers = db.prepare('SELECT id, name, app_type, is_current FROM providers').all();
  console.log('Providers:', JSON.stringify(providers, null, 2));
} catch (e) {
  console.log('Error:', e.message);
}

db.close();
