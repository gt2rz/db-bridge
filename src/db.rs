use anyhow::{Context, Result, bail};
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::time::{Duration, Instant};

pub const ROW_LIMIT: usize = 200;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ObjectKind {
    Table,
    View,
}

#[derive(Clone, Debug)]
pub struct DbObject {
    pub name: String,
    pub kind: ObjectKind,
    pub without_rowid: bool,
}

#[derive(Clone, Debug, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub keys: Vec<Value>,
    pub key_expr: Option<String>,
    pub elapsed: Duration,
    /// La sentencia modificó la base de datos (DML, DDL o `… RETURNING`).
    pub modified: bool,
    /// Filas totales de la tabla al explorarla página a página.
    pub total: Option<u64>,
}

impl QueryResult {
    pub fn is_editable(&self) -> bool {
        self.key_expr.is_some()
    }
}

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("no se pudo abrir {}", path.display()))?;
    conn.busy_timeout(Duration::from_millis(3_000))?;
    Ok(conn)
}

pub fn checkpoint(path: &Path) -> Result<()> {
    let conn = open(path)?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .context("no se pudo consolidar el WAL antes de enviar la base de datos")?;
    Ok(())
}

pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub fn list_objects(conn: &Connection) -> Result<Vec<DbObject>> {
    match list_objects_via_table_list(conn) {
        Ok(objects) => Ok(objects),
        Err(_) => list_objects_via_master(conn),
    }
}

fn list_objects_via_table_list(conn: &Connection) -> Result<Vec<DbObject>> {
    let mut stmt = conn.prepare("PRAGMA table_list")?;
    let mut rows = stmt.query([])?;
    let mut objects = Vec::new();
    while let Some(row) = rows.next()? {
        let schema: String = row.get(0)?;
        let name: String = row.get(1)?;
        let kind: String = row.get(2)?;
        let without_rowid: i64 = row.get(4)?;
        if schema != "main" || name.starts_with("sqlite_") {
            continue;
        }
        let kind = match kind.as_str() {
            "table" => ObjectKind::Table,
            "view" => ObjectKind::View,
            _ => continue,
        };
        objects.push(DbObject {
            name,
            kind,
            without_rowid: without_rowid != 0,
        });
    }
    objects.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(objects)
}

fn list_objects_via_master(conn: &Connection) -> Result<Vec<DbObject>> {
    let mut stmt = conn.prepare(
        "SELECT name, type FROM sqlite_master
         WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )?;
    let mut rows = stmt.query([])?;
    let mut objects = Vec::new();
    while let Some(row) = rows.next()? {
        let name: String = row.get(0)?;
        let kind: String = row.get(1)?;
        objects.push(DbObject {
            name,
            kind: if kind == "view" {
                ObjectKind::View
            } else {
                ObjectKind::Table
            },
            without_rowid: false,
        });
    }
    Ok(objects)
}

fn key_expression(conn: &Connection, object: &DbObject) -> Result<Option<String>> {
    if object.kind == ObjectKind::View {
        return Ok(None);
    }
    if !object.without_rowid {
        return Ok(Some("rowid".to_string()));
    }

    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", quote_ident(&object.name)))?;
    let mut rows = stmt.query([])?;
    let mut primary_keys: Vec<(i64, String)> = Vec::new();
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        let pk: i64 = row.get(5)?;
        if pk > 0 {
            primary_keys.push((pk, name));
        }
    }
    primary_keys.sort_by_key(|(position, _)| *position);

    Ok(match primary_keys.as_slice() {
        [(_, name)] => Some(quote_ident(name)),
        _ => None,
    })
}

/// `true` si la sentencia no modifica la base de datos. Si no compila, se trata como lectura:
/// ejecutarla solo producirá el error de sintaxis.
pub fn is_read_only(conn: &Connection, sql: &str) -> bool {
    conn.prepare(sql)
        .map(|stmt| stmt.readonly())
        .unwrap_or(true)
}

pub fn run_query(conn: &Connection, sql: &str) -> Result<QueryResult> {
    let started = Instant::now();
    let mut stmt = conn.prepare(sql).context("consulta SQL inválida")?;
    let column_count = stmt.column_count();
    let modified = !stmt.readonly();

    if column_count == 0 {
        drop(stmt);
        let affected = conn
            .execute(sql, [])
            .context("no se pudo ejecutar la sentencia")?;
        return Ok(QueryResult {
            columns: vec!["filas afectadas".to_string()],
            rows: vec![vec![Value::Integer(affected as i64)]],
            keys: Vec::new(),
            key_expr: None,
            elapsed: started.elapsed(),
            modified,
            total: None,
        });
    }

    let columns = stmt
        .column_names()
        .map(|name| name.to_string())
        .collect::<Vec<_>>();

    let mut rows = stmt.query([])?;
    let mut data = Vec::new();
    while let Some(row) = rows.next()? {
        let mut values = Vec::with_capacity(column_count);
        for index in 0..column_count {
            values.push(row.get::<_, Value>(index)?);
        }
        data.push(values);
    }

    Ok(QueryResult {
        columns,
        rows: data,
        keys: Vec::new(),
        key_expr: None,
        elapsed: started.elapsed(),
        modified,
        total: None,
    })
}

/// Consulta que muestra el editor al abrir una página de una tabla.
pub fn table_page_sql(object: &DbObject, page: usize) -> String {
    let name = quote_ident(&object.name);
    match page {
        0 => format!("SELECT * FROM {name} LIMIT {ROW_LIMIT};"),
        _ => format!(
            "SELECT * FROM {name} LIMIT {ROW_LIMIT} OFFSET {};",
            page * ROW_LIMIT
        ),
    }
}

/// Lee una página de la tabla y su número total de filas. Con clave, las filas van ordenadas
/// por ella para que las páginas no se solapen ni dejen huecos.
pub fn run_table_query(
    conn: &Connection,
    object: &DbObject,
    limit: usize,
    offset: usize,
) -> Result<QueryResult> {
    let started = Instant::now();
    let total = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM {}", quote_ident(&object.name)),
            [],
            |row| row.get::<_, i64>(0),
        )
        .context("no se pudo contar las filas")?;

    let keyed = match key_expression(conn, object)? {
        Some(key_expr) => query_with_key(conn, object, &key_expr, limit, offset).ok(),
        None => None,
    };
    let mut result = match keyed {
        Some(result) => result,
        None => {
            let sql = format!(
                "SELECT * FROM {} LIMIT {limit} OFFSET {offset}",
                quote_ident(&object.name)
            );
            let mut result = run_query(conn, &sql)?;
            result.key_expr = None;
            result
        }
    };
    result.total = Some(total.max(0) as u64);
    result.elapsed = started.elapsed();
    Ok(result)
}

fn query_with_key(
    conn: &Connection,
    object: &DbObject,
    key_expr: &str,
    limit: usize,
    offset: usize,
) -> Result<QueryResult> {
    let sql = format!(
        "SELECT {key_expr} AS __db_key, * FROM {} ORDER BY {key_expr} LIMIT {limit} OFFSET {offset}",
        quote_ident(&object.name),
    );
    let mut stmt = conn.prepare(&sql)?;
    let column_count = stmt.column_count();
    if column_count < 2 {
        bail!("la tabla no expone filas consultables");
    }

    let mut columns = stmt
        .column_names()
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    if columns.first().map(String::as_str) != Some("__db_key") {
        bail!("no se pudo identificar la clave de la fila");
    }
    columns.remove(0);

    let mut rows = stmt.query([])?;
    let mut data = Vec::new();
    let mut keys = Vec::new();
    while let Some(row) = rows.next()? {
        let key = row.get::<_, Value>(0)?;
        if matches!(key, Value::Null) {
            bail!("la tabla contiene filas sin clave");
        }
        let mut values = Vec::with_capacity(column_count - 1);
        for index in 1..column_count {
            values.push(row.get::<_, Value>(index)?);
        }
        keys.push(key);
        data.push(values);
    }

    Ok(QueryResult {
        columns,
        rows: data,
        keys,
        key_expr: Some(key_expr.to_string()),
        elapsed: Duration::ZERO,
        modified: false,
        total: None,
    })
}

pub fn update_cell(
    conn: &Connection,
    table: &str,
    key_expr: &str,
    key: &Value,
    column: &str,
    new_value: &Value,
) -> Result<usize> {
    let sql = format!(
        "UPDATE {} SET {} = ?1 WHERE {} = ?2",
        quote_ident(table),
        quote_ident(column),
        key_expr
    );
    conn.execute(&sql, rusqlite::params![new_value, key])
        .context("no se pudo actualizar la celda")
}

/// Tamaño en disco del archivo principal y de su `-wal` (0 si no existe).
pub fn disk_size(path: &Path) -> Option<(u64, u64)> {
    let main = std::fs::metadata(path).ok()?.len();
    let wal = std::fs::metadata(format!("{}-wal", path.display()))
        .map(|meta| meta.len())
        .unwrap_or(0);
    Some((main, wal))
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64 / 1024.0;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

pub fn value_display(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_string(),
        Value::Integer(number) => number.to_string(),
        Value::Real(number) => number.to_string(),
        Value::Text(text) => text.clone(),
        Value::Blob(bytes) => format!("<BLOB {} bytes>", bytes.len()),
    }
}

pub fn value_json(value: &Value) -> serde_json::Value {
    use serde_json::Value as Json;
    match value {
        Value::Null => Json::Null,
        Value::Integer(number) => Json::from(*number),
        Value::Real(number) => serde_json::Number::from_f64(*number)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        Value::Text(text) => Json::String(text.clone()),
        Value::Blob(bytes) => Json::String(format!("<BLOB {} bytes>", bytes.len())),
    }
}

pub fn to_csv(result: &QueryResult) -> String {
    let mut output = String::new();
    let header = result
        .columns
        .iter()
        .map(|column| csv_field(column))
        .collect::<Vec<_>>()
        .join(",");
    output.push_str(&header);
    output.push('\n');
    for row in &result.rows {
        let line = row
            .iter()
            .map(|value| csv_field(&value_display(value)))
            .collect::<Vec<_>>()
            .join(",");
        output.push_str(&line);
        output.push('\n');
    }
    output
}

fn csv_field(value: &str) -> String {
    if value
        .chars()
        .any(|character| matches!(character, ',' | '"' | '\n' | '\r'))
    {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

pub fn to_json(result: &QueryResult) -> serde_json::Value {
    let rows = result
        .rows
        .iter()
        .map(|row| {
            let mut object = serde_json::Map::new();
            for (index, column) in result.columns.iter().enumerate() {
                let value = row
                    .get(index)
                    .map(value_json)
                    .unwrap_or(serde_json::Value::Null);
                object.insert(column.clone(), value);
            }
            serde_json::Value::Object(object)
        })
        .collect::<Vec<_>>();
    serde_json::Value::Array(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE items (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                qty INTEGER,
                price REAL
            );
            INSERT INTO items (name, qty, price) VALUES
                ('teclado', 3, 19.5),
                ('mouse', NULL, 9.0);
            CREATE TABLE kv (k TEXT PRIMARY KEY, v TEXT) WITHOUT ROWID;
            INSERT INTO kv VALUES ('a', '1');
            CREATE VIEW item_names AS SELECT name FROM items;
            "#,
        )
        .unwrap();
        conn
    }

    fn object(conn: &Connection, name: &str) -> DbObject {
        list_objects(conn)
            .unwrap()
            .into_iter()
            .find(|object| object.name == name)
            .unwrap()
    }

    #[test]
    fn lists_tables_and_views() {
        let conn = memory();
        let objects = list_objects(&conn).unwrap();
        let names = objects
            .iter()
            .map(|object| object.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["item_names", "items", "kv"]);
        assert_eq!(object(&conn, "item_names").kind, ObjectKind::View);
        assert!(object(&conn, "kv").without_rowid);
    }

    #[test]
    fn table_query_is_editable_and_updates_cells() {
        let conn = memory();
        let items = object(&conn, "items");
        let result = run_table_query(&conn, &items, 10, 0).unwrap();
        assert!(result.is_editable());
        assert_eq!(result.columns, vec!["id", "name", "qty", "price"]);
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.keys[0], Value::Integer(1));

        let updated = update_cell(
            &conn,
            &items.name,
            result.key_expr.as_deref().unwrap(),
            &result.keys[0],
            "name",
            &Value::Text("teclado mecánico".to_string()),
        )
        .unwrap();
        assert_eq!(updated, 1);

        let reloaded = run_table_query(&conn, &items, 10, 0).unwrap();
        assert_eq!(
            reloaded.rows[0][1],
            Value::Text("teclado mecánico".to_string())
        );
    }

    #[test]
    fn table_pages_are_ordered_and_report_the_total() {
        let conn = memory();
        conn.execute_batch(
            "INSERT INTO items (name) VALUES ('c'), ('d'), ('e');
             UPDATE items SET qty = 1 WHERE id = 1;",
        )
        .unwrap();
        let items = object(&conn, "items");

        let first = run_table_query(&conn, &items, 2, 0).unwrap();
        let last = run_table_query(&conn, &items, 2, 4).unwrap();
        assert_eq!(first.total, Some(5));
        assert_eq!(first.keys, vec![Value::Integer(1), Value::Integer(2)]);
        assert_eq!(last.keys, vec![Value::Integer(5)]);
        assert!(last.is_editable());

        let beyond = run_table_query(&conn, &items, 2, 10).unwrap();
        assert!(beyond.rows.is_empty());
        assert_eq!(beyond.total, Some(5));
    }

    #[test]
    fn views_are_paged_without_key() {
        let conn = memory();
        let view = object(&conn, "item_names");
        let page = run_table_query(&conn, &view, 1, 1).unwrap();
        assert_eq!(page.rows, vec![vec![Value::Text("mouse".to_string())]]);
        assert_eq!(page.total, Some(2));
        assert!(!page.is_editable());
    }

    #[test]
    fn page_sql_adds_offset_after_the_first_page() {
        let conn = memory();
        let items = object(&conn, "items");
        assert_eq!(
            table_page_sql(&items, 0),
            format!("SELECT * FROM \"items\" LIMIT {ROW_LIMIT};")
        );
        assert_eq!(
            table_page_sql(&items, 2),
            format!(
                "SELECT * FROM \"items\" LIMIT {ROW_LIMIT} OFFSET {};",
                2 * ROW_LIMIT
            )
        );
    }

    #[test]
    fn without_rowid_table_uses_primary_key() {
        let conn = memory();
        let kv = object(&conn, "kv");
        let result = run_table_query(&conn, &kv, 10, 0).unwrap();
        assert!(result.is_editable());
        assert_eq!(result.key_expr.as_deref(), Some("\"k\""));
        assert_eq!(result.keys[0], Value::Text("a".to_string()));

        let updated = update_cell(
            &conn,
            &kv.name,
            result.key_expr.as_deref().unwrap(),
            &result.keys[0],
            "v",
            &Value::Text("2".to_string()),
        )
        .unwrap();
        assert_eq!(updated, 1);
    }

    #[test]
    fn views_are_read_only() {
        let conn = memory();
        let view = object(&conn, "item_names");
        let result = run_table_query(&conn, &view, 10, 0).unwrap();
        assert!(!result.is_editable());
        assert_eq!(result.key_expr, None);
        assert_eq!(result.rows.len(), 2);
    }

    #[test]
    fn identifiers_are_quoted() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(r#"CREATE TABLE "we""ird" ("col""name" TEXT);"#)
            .unwrap();
        conn.execute(r#"INSERT INTO "we""ird" ("col""name") VALUES ('ok');"#, [])
            .unwrap();
        let weird = object(&conn, "we\"ird");
        let result = run_table_query(&conn, &weird, 10, 0).unwrap();
        assert_eq!(result.columns, vec!["col\"name"]);
        assert_eq!(result.rows[0][0], Value::Text("ok".to_string()));
        update_cell(
            &conn,
            &weird.name,
            result.key_expr.as_deref().unwrap(),
            &result.keys[0],
            "col\"name",
            &Value::Text("still ok".to_string()),
        )
        .unwrap();
        let reloaded = run_table_query(&conn, &weird, 10, 0).unwrap();
        assert_eq!(reloaded.rows[0][0], Value::Text("still ok".to_string()));
    }

    #[test]
    fn raw_query_is_read_only() {
        let conn = memory();
        let result = run_query(&conn, "SELECT name, qty FROM items ORDER BY id").unwrap();
        assert_eq!(result.columns, vec!["name", "qty"]);
        assert!(!result.is_editable());
    }

    #[test]
    fn csv_escapes_special_characters() {
        let conn = Connection::open_in_memory().unwrap();
        let result = run_query(
            &conn,
            r#"SELECT 'a,b' AS a, 'say "hi"' AS b, 'line
break' AS c"#,
        )
        .unwrap();
        let csv = to_csv(&result);
        assert!(csv.starts_with("a,b,c\n"));
        assert!(csv.contains("\"a,b\",\"say \"\"hi\"\"\",\"line\nbreak\""));
    }

    #[test]
    fn json_preserves_types() {
        let conn = memory();
        let result = run_query(
            &conn,
            "SELECT id, name, price, NULL AS missing FROM items ORDER BY id LIMIT 1",
        )
        .unwrap();
        let json = to_json(&result);
        assert_eq!(json[0]["id"], serde_json::json!(1));
        assert_eq!(json[0]["name"], serde_json::json!("teclado"));
        assert_eq!(json[0]["price"], serde_json::json!(19.5));
        assert_eq!(json[0]["missing"], serde_json::Value::Null);
    }

    #[test]
    fn checkpoint_truncates_the_wal_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("app.db");
        std::fs::write(&path, b"").unwrap();

        let writer = open(&path).unwrap();
        writer.pragma_update(None, "journal_mode", "WAL").unwrap();
        writer
            .execute_batch(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT);
                 INSERT INTO t VALUES (1, 'a');",
            )
            .unwrap();

        let wal = std::path::PathBuf::from(format!("{}-wal", path.display()));
        assert!(wal.exists());

        checkpoint(&path).unwrap();
        let size = std::fs::metadata(&wal).map(|meta| meta.len()).unwrap_or(0);
        assert_eq!(size, 0);
    }

    #[test]
    fn mutation_statement_reports_affected_rows() {
        let conn = memory();
        let result = run_query(&conn, "UPDATE items SET qty = 7 WHERE name = 'mouse'").unwrap();
        assert_eq!(result.rows[0][0], Value::Integer(1));
        assert!(result.modified);
    }

    #[test]
    fn detects_statements_that_modify_the_database() {
        let conn = memory();
        assert!(!run_query(&conn, "SELECT * FROM items").unwrap().modified);
        assert!(
            run_query(&conn, "DELETE FROM items RETURNING name")
                .unwrap()
                .modified
        );
        assert!(is_read_only(&conn, "SELECT count(*) FROM items"));
        assert!(!is_read_only(&conn, "DELETE FROM items"));
        assert!(!is_read_only(&conn, "DROP TABLE items"));
    }

    #[test]
    fn formats_file_sizes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn disk_size_includes_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.db");
        std::fs::write(&path, vec![0; 4096]).unwrap();
        assert_eq!(disk_size(&path), Some((4096, 0)));
        std::fs::write(dir.path().join("app.db-wal"), vec![0; 100]).unwrap();
        assert_eq!(disk_size(&path), Some((4096, 100)));
        assert_eq!(disk_size(&dir.path().join("missing.db")), None);
    }
}
