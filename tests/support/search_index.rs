// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

//! Schema-4 search indexes against the independently replayed base Python data.
//! Only the version, captured-window column and Porter objects may differ.
use std::path::Path;
use std::sync::LazyLock;

use rusqlite::{Connection, OpenFlags, types::ValueRef};
use serde_json::{Value, json};

static ORACLE: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(include_str!("../golden/search-indexes.json")).unwrap());

fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn rows(connection: &Connection, sql: &str) -> Result<Vec<Value>, String> {
    let mut statement = connection.prepare(sql).map_err(|e| e.to_string())?;
    let count = statement.column_count();
    let mut cursor = statement.query([]).map_err(|e| e.to_string())?;
    let mut result = Vec::new();
    while let Some(row) = cursor.next().map_err(|e| e.to_string())? {
        let cells = (0..count)
            .map(|column| {
                Ok(match row.get_ref(column).map_err(|e| e.to_string())? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(n) => json!(n),
                    ValueRef::Real(n) => json!(n),
                    ValueRef::Text(s) => json!(std::str::from_utf8(s).map_err(|e| e.to_string())?),
                    ValueRef::Blob(bytes) => {
                        json!({"blob": bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()})
                    }
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        result.push(json!(cells));
    }
    Ok(result)
}

fn snapshot(path: &Path) -> Result<Value, String> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let health = rows(&connection, "PRAGMA quick_check")?;
    if health != vec![json!(["ok"])] {
        return Err(format!("SQLite quick_check: {health:?}"));
    }
    let schema = rows(
        &connection,
        "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name",
    )?;
    let mut tables = serde_json::Map::new();
    for object in &schema {
        if object[0] != "table" {
            continue;
        }
        let name = object[1].as_str().unwrap();
        let columns: Vec<Value> =
            rows(&connection, &format!("PRAGMA table_info({})", quoted(name)))?
                .iter()
                .map(|column| column[1].clone())
                .collect();
        let order = (1..=columns.len())
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let data = rows(
            &connection,
            &format!("SELECT * FROM {} ORDER BY {order}", quoted(name)),
        )?;
        tables.insert(name.into(), json!({"columns": columns, "rows": data}));
    }
    Ok(json!({"schema": schema, "tables": tables}))
}

fn expected(base: &Value) -> Value {
    let mut want = base.clone();
    let porter = want.as_object_mut().unwrap().remove("porter").unwrap();
    // These explicit additions leave every original column, value and FTS
    // posting checked. The original fixtures have no captured supersession date.
    let titles = &mut want["tables"]["titles"];
    let column = titles["columns"]
        .as_array()
        .unwrap()
        .iter()
        .position(|name| name == "retrieved")
        .unwrap()
        + 1;
    titles["columns"]
        .as_array_mut()
        .unwrap()
        .insert(column, json!("superseded_from"));
    for row in titles["rows"].as_array_mut().unwrap() {
        row.as_array_mut().unwrap().insert(column, Value::Null);
    }
    for row in want["tables"]["meta"]["rows"].as_array_mut().unwrap() {
        if row[0] == "schema" {
            assert_eq!(row[1], "2", "the oracle must remain the base Python schema");
            row[1] = json!("4");
        }
    }
    let schema = want["schema"].as_array_mut().unwrap();
    for object in schema.iter_mut() {
        if object[1] == "titles" {
            object[3] = json!(object[3].as_str().unwrap().replace(
                "retrieved TEXT, register_page",
                "retrieved TEXT, superseded_from TEXT, register_page"
            ));
        }
    }
    schema.extend(porter["schema"].as_array().unwrap().iter().cloned());
    schema.sort_by(|a, b| (a[0].as_str(), a[1].as_str()).cmp(&(b[0].as_str(), b[1].as_str())));
    want["tables"]
        .as_object_mut()
        .unwrap()
        .extend(porter["tables"].as_object().unwrap().clone());
    want
}

/// None means this is not one of the independently replayed valid indexes;
/// callers must retain its original file hash check, including malformed files.
pub fn check(index: u64, relative: &str, path: &Path) -> Option<Result<(), String>> {
    let base = ORACLE["snapshots"].get(format!("{index}:{relative}"))?;
    Some(snapshot(path).and_then(|actual| {
        let want = expected(base);
        if actual == want {
            return Ok(());
        }
        let tables: Vec<_> = actual["tables"]
            .as_object()
            .unwrap()
            .keys()
            .chain(want["tables"].as_object().unwrap().keys())
            .filter(|name| actual["tables"][*name] != want["tables"][*name])
            .collect();
        Err(format!(
            "Python logical oracle differs: schema={}, tables={tables:?}",
            actual["schema"] != want["schema"]
        ))
    }))
}
