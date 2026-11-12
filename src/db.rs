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

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnInfo {
    pub name: String,
    pub decl_type: String,
    pub not_null: bool,
    pub default: Option<String>,
    /// Posición en la clave primaria (1, 2…) o 0 si no forma parte de ella.
    pub primary_key: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IndexInfo {
    pub name: String,
    pub unique: bool,
    /// `c` creado con CREATE INDEX, `u` por una restricción UNIQUE, `pk` por la clave primaria.
    pub origin: String,
    pub partial: bool,
    /// Columnas indexadas; las expresiones aparecen como `<expresión>`.
    pub columns: Vec<String>,
    pub sql: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ForeignKey {
    pub from: Vec<String>,
    pub table: String,
    pub to: Vec<String>,
    pub on_update: String,
    pub on_delete: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TableSchema {
    pub columns: Vec<ColumnInfo>,
    pub indexes: Vec<IndexInfo>,
    pub foreign_keys: Vec<ForeignKey>,
    /// Nombre y sentencia de cada trigger asociado.
    pub triggers: Vec<(String, String)>,
    /// `CREATE TABLE` / `CREATE VIEW` original.
    pub sql: Option<String>,
}

pub fn table_schema(conn: &Connection, name: &str) -> Result<TableSchema> {
    let mut schema = TableSchema::default();

    let mut stmt =
        conn.prepare("SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_info(?1)")?;
    schema.columns = stmt
        .query_map([name], |row| {
            Ok(ColumnInfo {
                name: row.get(0)?,
                decl_type: row.get(1)?,
                not_null: row.get::<_, i64>(2)? != 0,
                default: row.get(3)?,
                primary_key: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()
        .context("no se pudieron leer las columnas")?;

    let mut stmt = conn.prepare(
        "SELECT l.name, l.\"unique\", l.origin, l.partial, m.sql
         FROM pragma_index_list(?1) AS l
         LEFT JOIN sqlite_master AS m ON m.type = 'index' AND m.name = l.name
         ORDER BY l.name",
    )?;
    let indexes = stmt
        .query_map([name], |row| {
            Ok(IndexInfo {
                name: row.get(0)?,
                unique: row.get::<_, i64>(1)? != 0,
                origin: row.get(2)?,
                partial: row.get::<_, i64>(3)? != 0,
                columns: Vec::new(),
                sql: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("no se pudieron leer los índices")?;
    let mut stmt = conn.prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")?;
    for mut index in indexes {
        index.columns = stmt
            .query_map([index.name.as_str()], |row| {
                Ok(row
                    .get::<_, Option<String>>(0)?
                    .unwrap_or_else(|| "<expresión>".to_string()))
            })?
            .collect::<rusqlite::Result<_>>()?;
        schema.indexes.push(index);
    }

    let mut stmt = conn.prepare(
        "SELECT id, \"table\", \"from\", \"to\", on_update, on_delete
         FROM pragma_foreign_key_list(?1) ORDER BY id, seq",
    )?;
    let mut rows = stmt.query([name])?;
    let mut current_id = None;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        // `to` es NULL cuando la FK apunta implícitamente a la clave primaria.
        let to = row
            .get::<_, Option<String>>(3)?
            .unwrap_or_else(|| "(clave primaria)".to_string());
        if current_id != Some(id) {
            current_id = Some(id);
            schema.foreign_keys.push(ForeignKey {
                from: Vec::new(),
                table: row.get(1)?,
                to: Vec::new(),
                on_update: row.get(4)?,
                on_delete: row.get(5)?,
            });
        }
        if let Some(key) = schema.foreign_keys.last_mut() {
            key.from.push(row.get(2)?);
            key.to.push(to);
        }
    }

    let mut stmt = conn.prepare(
        "SELECT name, sql FROM sqlite_master WHERE type = 'trigger' AND tbl_name = ?1 ORDER BY name",
    )?;
    schema.triggers = stmt
        .query_map([name], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()
        .context("no se pudieron leer los triggers")?;

    schema.sql = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type IN ('table', 'view') AND name = ?1",
            [name],
            |row| row.get(0),
        )
        .ok();

    Ok(schema)
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

pub fn delete_row(conn: &Connection, table: &str, key_expr: &str, key: &Value) -> Result<usize> {
    let sql = format!("DELETE FROM {} WHERE {} = ?1", quote_ident(table), key_expr);
    conn.execute(&sql, [key])
        .context("no se pudo eliminar la fila")
}

/// Inserta una fila con los valores indicados; el resto de columnas toma su valor por
/// defecto. Devuelve el `rowid` de la fila nueva (0 en tablas `WITHOUT ROWID`).
pub fn insert_row(conn: &Connection, table: &str, values: &[(String, Value)]) -> Result<i64> {
    let sql = if values.is_empty() {
        format!("INSERT INTO {} DEFAULT VALUES", quote_ident(table))
    } else {
        let columns = values
            .iter()
            .map(|(column, _)| quote_ident(column))
            .collect::<Vec<_>>()
            .join(", ");
        let placeholders = (1..=values.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "INSERT INTO {} ({columns}) VALUES ({placeholders})",
            quote_ident(table)
        )
    };
    conn.execute(
        &sql,
        rusqlite::params_from_iter(values.iter().map(|(_, value)| value)),
    )
    .context("no se pudo insertar la fila")?;
    Ok(conn.last_insert_rowid())
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
    fn reads_columns_indexes_foreign_keys_and_triggers() {
        let conn = memory();
        conn.execute_batch(
            "CREATE TABLE orders (
                id INTEGER PRIMARY KEY,
                item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
                code TEXT UNIQUE,
                status TEXT DEFAULT 'pending'
            );
            CREATE INDEX orders_status ON orders (status) WHERE status IS NOT NULL;
            CREATE INDEX orders_lower ON orders (lower(code));
            CREATE TRIGGER orders_touch AFTER UPDATE ON orders BEGIN SELECT 1; END;",
        )
        .unwrap();

        let schema = table_schema(&conn, "orders").unwrap();
        assert_eq!(
            schema.columns[1],
            ColumnInfo {
                name: "item_id".to_string(),
                decl_type: "INTEGER".to_string(),
                not_null: true,
                default: None,
                primary_key: 0,
            }
        );
        assert_eq!(schema.columns[0].primary_key, 1);
        assert_eq!(schema.columns[3].default.as_deref(), Some("'pending'"));

        let names: Vec<_> = schema.indexes.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["orders_lower", "orders_status", "sqlite_autoindex_orders_1"]
        );
        assert_eq!(schema.indexes[0].columns, vec!["<expresión>"]);
        assert!(schema.indexes[1].partial);
        assert!(schema.indexes[2].unique);
        assert_eq!(schema.indexes[2].origin, "u");
        assert!(schema.indexes[2].sql.is_none());

        assert_eq!(
            schema.foreign_keys,
            vec![ForeignKey {
                from: vec!["item_id".to_string()],
                table: "items".to_string(),
                to: vec!["id".to_string()],
                on_update: "NO ACTION".to_string(),
                on_delete: "CASCADE".to_string(),
            }]
        );
        assert_eq!(schema.triggers.len(), 1);
        assert_eq!(schema.triggers[0].0, "orders_touch");
        assert!(schema.sql.unwrap().starts_with("CREATE TABLE orders"));
    }

    #[test]
    fn view_schema_has_columns_and_sql() {
        let conn = memory();
        let schema = table_schema(&conn, "item_names").unwrap();
        assert_eq!(schema.columns.len(), 1);
        assert!(schema.indexes.is_empty());
        assert!(schema.sql.unwrap().starts_with("CREATE VIEW"));
    }

    #[test]
    fn inserts_with_defaults_and_deletes_by_key() {
        let conn = memory();
        let id = insert_row(
            &conn,
            "items",
            &[
                ("name".to_string(), Value::Text("cable".to_string())),
                ("qty".to_string(), Value::Text("7".to_string())),
            ],
        )
        .unwrap();
        assert_eq!(id, 3);
        // La afinidad INTEGER convierte el texto escrito en el formulario.
        let qty: Value = conn
            .query_row("SELECT qty FROM items WHERE id = 3", [], |row| row.get(0))
            .unwrap();
        assert_eq!(qty, Value::Integer(7));

        assert!(insert_row(&conn, "items", &[]).is_err(), "name es NOT NULL");

        assert_eq!(
            delete_row(&conn, "items", "rowid", &Value::Integer(3)).unwrap(),
            1
        );
        assert_eq!(
            delete_row(&conn, "kv", "\"k\"", &Value::Text("a".to_string())).unwrap(),
            1
        );
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining, 2);
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
