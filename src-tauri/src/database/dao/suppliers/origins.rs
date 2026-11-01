//! Private historical recovery records for configurations consolidated by import.
//! This data never supplies routing or IPC views. Credentials remain inside the
//! same protected SQLite store, just as the pre-mutation recovery snapshot does.
use super::*;

const ORIGIN_BYTES_LIMIT: usize = 4 * 1024 * 1024;

pub(super) fn retain_merge_origins_on_conn(
    conn: &Connection,
    keep_id: &str,
    remove_id: &str,
) -> Result<(), AppError> {
    let read = |id: &str| -> Result<Vec<Value>, AppError> {
        let raw: String = conn.query_row(
            "SELECT import_origins_json FROM suppliers WHERE id=?1",
            [id],
            |row| row.get(0),
        )?;
        if raw.len() > ORIGIN_BYTES_LIMIT {
            return Err(AppError::InvalidInput(
                "supplier_origin_history_too_large".into(),
            ));
        }
        Ok(serde_json::from_str(&raw)?)
    };
    let mut origins = read(keep_id)?;
    if origins.is_empty() {
        origins.push(capture_origin(conn, keep_id)?);
    }
    origins.extend(read(remove_id)?);
    let removed = capture_origin(conn, remove_id)?;
    if !origins.contains(&removed) {
        origins.push(removed);
    }
    let encoded = serde_json::to_string(&origins)?;
    if encoded.len() > ORIGIN_BYTES_LIMIT {
        return Err(AppError::InvalidInput(
            "supplier_origin_history_too_large".into(),
        ));
    }
    conn.execute(
        "UPDATE suppliers SET import_origins_json=?1 WHERE id=?2",
        params![encoded, keep_id],
    )?;
    Ok(())
}

fn capture_origin(conn: &Connection, id: &str) -> Result<Value, AppError> {
    Ok(serde_json::json!({
        "supplier":dump_rows(conn,"SELECT id,name,base_url,api_key,enabled,priority,source,provenance_json,icon,icon_color,created_at,updated_at FROM suppliers WHERE id=?1",id)?,
        "endpoints":dump_rows(conn,"SELECT * FROM supplier_endpoints WHERE supplier_id=?1 ORDER BY id",id)?,
        "models":dump_rows(conn,"SELECT m.* FROM supplier_models m JOIN supplier_endpoints e ON e.id=m.endpoint_id WHERE e.supplier_id=?1 ORDER BY m.id",id)?,
        "projections":dump_rows(conn,"SELECT p.* FROM supplier_runtime_projections p JOIN supplier_endpoints e ON e.id=p.endpoint_id WHERE e.supplier_id=?1 ORDER BY p.endpoint_id",id)?
    }))
}

fn dump_rows(conn: &Connection, sql: &str, id: &str) -> Result<Vec<Value>, AppError> {
    let mut statement = conn.prepare(sql)?;
    let names: Vec<String> = statement
        .column_names()
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut rows = statement.query([id])?;
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        let mut object = serde_json::Map::new();
        for (index, name) in names.iter().enumerate() {
            use rusqlite::types::ValueRef;
            let value = match row.get_ref(index)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(value) => serde_json::json!(value),
                ValueRef::Real(value) => serde_json::json!(value),
                ValueRef::Text(value) => Value::String(
                    std::str::from_utf8(value)
                        .map_err(|_| AppError::InvalidInput("supplier_origin_text_invalid".into()))?
                        .to_string(),
                ),
                ValueRef::Blob(value) => serde_json::json!({"blobHex":hex::encode(value)}),
            };
            object.insert(name.clone(), value);
        }
        result.push(Value::Object(object));
    }
    Ok(result)
}
