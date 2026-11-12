//! Comparación entre una línea base (copia fijada de la BD) y su estado actual: qué filas se
//! insertaron, eliminaron o modificaron en cada tabla.

use crate::db::{self, quote_ident};
use anyhow::{Context, Result};
use rusqlite::Connection;
use rusqlite::types::Value;
use std::path::Path;

/// Filas que se muestran como máximo por tabla y tipo de cambio; los contadores son exactos.
pub const MAX_ROWS: usize = 200;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TableChange {
    Added,
    Removed,
    Modified,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowSet {
    pub count: u64,
    pub rows: Vec<Vec<Value>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UpdatedRow {
    pub before: Vec<Value>,
    pub after: Vec<Value>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UpdateSet {
    pub count: u64,
    pub rows: Vec<UpdatedRow>,
}

#[derive(Clone, Debug)]
pub struct TableDiff {
    pub name: String,
    pub change: TableChange,
    /// Columnas comparadas: las comunes a ambas versiones, o todas si la tabla es nueva o se
    /// eliminó.
    pub columns: Vec<String>,
    pub schema_changed: bool,
    pub inserted: RowSet,
    pub deleted: RowSet,
    pub updated: UpdateSet,
}

/// Tablas con cambios, en orden alfabético.
#[derive(Clone, Debug, Default)]
pub struct DatabaseDiff {
    pub tables: Vec<TableDiff>,
}

impl DatabaseDiff {
    pub fn totals(&self) -> (u64, u64, u64) {
        self.tables
            .iter()
            .fold((0, 0, 0), |(ins, del, upd), table| {
                (
                    ins + table.inserted.count,
                    del + table.deleted.count,
                    upd + table.updated.count,
                )
            })
    }
}

/// Copia la BD (y su `-wal`, si lo hay) a `destination` byte a byte. No se usa `VACUUM INTO`
/// porque puede renumerar los `rowid` y la comparación los usa como clave.
pub fn snapshot(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("no se pudo crear {}", parent.display()))?;
    }
    for suffix in ["", "-wal", "-shm"] {
        let from = format!("{}{suffix}", source.display());
        let to = format!("{}{suffix}", destination.display());
        if Path::new(&from).is_file() {
            std::fs::copy(&from, &to).with_context(|| format!("no se pudo copiar {from}"))?;
        } else {
            let _ = std::fs::remove_file(&to);
        }
    }
    Ok(())
}

struct TableInfo {
    name: String,
    columns: Vec<String>,
    /// Columnas de la clave primaria en orden, o `rowid` si la tabla no declara ninguna.
    key: Vec<String>,
}

pub fn compare(current: &Path, baseline: &Path) -> Result<DatabaseDiff> {
    let conn = db::open(current)?;
    conn.execute(
        "ATTACH DATABASE ?1 AS base",
        [baseline.to_string_lossy().as_ref()],
    )
    .context("no se pudo abrir la línea base")?;

    let now = tables(&conn, "main")?;
    let before = tables(&conn, "base")?;
    let mut diff = DatabaseDiff::default();

    for table in &now {
        let entry = match before.iter().find(|old| old.name == table.name) {
            Some(old) => compare_table(&conn, table, old)?,
            None => TableDiff {
                name: table.name.clone(),
                change: TableChange::Added,
                columns: table.columns.clone(),
                schema_changed: false,
                inserted: whole_table(&conn, "main", table)?,
                deleted: RowSet::default(),
                updated: UpdateSet::default(),
            },
        };
        if entry.change != TableChange::Modified
            || entry.schema_changed
            || entry.inserted.count + entry.deleted.count + entry.updated.count > 0
        {
            diff.tables.push(entry);
        }
    }
    for old in before
        .iter()
        .filter(|old| !now.iter().any(|table| table.name == old.name))
    {
        diff.tables.push(TableDiff {
            name: old.name.clone(),
            change: TableChange::Removed,
            columns: old.columns.clone(),
            schema_changed: false,
            inserted: RowSet::default(),
            deleted: whole_table(&conn, "base", old)?,
            updated: UpdateSet::default(),
        });
    }
    diff.tables.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(diff)
}

fn tables(conn: &Connection, schema: &str) -> Result<Vec<TableInfo>> {
    let names = {
        let mut stmt = conn.prepare(&format!(
            "SELECT name FROM {schema}.sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
        ))?;
        stmt.query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut tables = Vec::with_capacity(names.len());
    for name in names {
        let mut stmt = conn.prepare("SELECT name, pk FROM pragma_table_info(?1, ?2)")?;
        let mut columns = Vec::new();
        let mut primary_key = Vec::new();
        let mut rows = stmt.query([name.as_str(), schema])?;
        while let Some(row) = rows.next()? {
            let column: String = row.get(0)?;
            let position: i64 = row.get(1)?;
            if position > 0 {
                primary_key.push((position, column.clone()));
            }
            columns.push(column);
        }
        primary_key.sort_by_key(|(position, _)| *position);
        let key = if primary_key.is_empty() {
            vec!["rowid".to_string()]
        } else {
            primary_key.into_iter().map(|(_, column)| column).collect()
        };
        tables.push(TableInfo { name, columns, key });
    }
    Ok(tables)
}

fn whole_table(conn: &Connection, schema: &str, table: &TableInfo) -> Result<RowSet> {
    let columns = select_list("t", &table.columns);
    let query = format!(
        "SELECT {columns} FROM {schema}.{} AS t",
        quote_ident(&table.name)
    );
    row_set(conn, &query, "")
}

fn compare_table(conn: &Connection, now: &TableInfo, old: &TableInfo) -> Result<TableDiff> {
    let columns: Vec<String> = now
        .columns
        .iter()
        .filter(|column| old.columns.contains(column))
        .cloned()
        .collect();
    let schema_changed = now.columns != old.columns;
    let table = quote_ident(&now.name);

    // La clave debe existir igual en ambas versiones; si cambió, se comparan filas completas.
    let key_usable = now.key == old.key
        && now
            .key
            .iter()
            .all(|key| key == "rowid" || columns.contains(key));

    let (inserted, deleted, updated) = if key_usable {
        let key: Vec<String> = now.key.iter().map(|key| key_ident(key)).collect();
        let join = key
            .iter()
            .map(|key| format!("b.{key} IS m.{key}"))
            .collect::<Vec<_>>()
            .join(" AND ");
        let order = |alias: &str| {
            let keys = key
                .iter()
                .map(|key| format!("{alias}.{key}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(" ORDER BY {keys}")
        };

        let inserted = row_set(
            conn,
            &format!(
                "SELECT {} FROM main.{table} AS m WHERE NOT EXISTS \
                 (SELECT 1 FROM base.{table} AS b WHERE {join})",
                select_list("m", &columns)
            ),
            &order("m"),
        )?;
        let deleted = row_set(
            conn,
            &format!(
                "SELECT {} FROM base.{table} AS b WHERE NOT EXISTS \
                 (SELECT 1 FROM main.{table} AS m WHERE {join})",
                select_list("b", &columns)
            ),
            &order("b"),
        )?;

        let differs = columns
            .iter()
            .filter(|column| !now.key.contains(column))
            .map(|column| {
                let column = quote_ident(column);
                format!("m.{column} IS NOT b.{column}")
            })
            .collect::<Vec<_>>();
        let updated = if differs.is_empty() {
            UpdateSet::default()
        } else {
            let query = format!(
                "SELECT {}, {} FROM main.{table} AS m JOIN base.{table} AS b ON {join} WHERE {}",
                select_list("b", &columns),
                select_list("m", &columns),
                differs.join(" OR ")
            );
            let split = columns.len();
            let RowSet { count, rows } = row_set(conn, &query, &order("m"))?;
            UpdateSet {
                count,
                rows: rows
                    .into_iter()
                    .map(|mut before| {
                        let after = before.split_off(split);
                        UpdatedRow { before, after }
                    })
                    .collect(),
            }
        };
        (inserted, deleted, updated)
    } else {
        let current = format!(
            "SELECT {} FROM main.{table} AS m",
            select_list("m", &columns)
        );
        let previous = format!(
            "SELECT {} FROM base.{table} AS b",
            select_list("b", &columns)
        );
        (
            row_set(conn, &format!("{current} EXCEPT {previous}"), "")?,
            row_set(conn, &format!("{previous} EXCEPT {current}"), "")?,
            UpdateSet::default(),
        )
    };

    Ok(TableDiff {
        name: now.name.clone(),
        change: TableChange::Modified,
        columns,
        schema_changed,
        inserted,
        deleted,
        updated,
    })
}

fn key_ident(key: &str) -> String {
    if key == "rowid" {
        key.to_string()
    } else {
        quote_ident(key)
    }
}

fn select_list(alias: &str, columns: &[String]) -> String {
    if columns.is_empty() {
        return "NULL".to_string();
    }
    columns
        .iter()
        .map(|column| format!("{alias}.{}", quote_ident(column)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Cuenta las filas de `query` y lee las primeras `MAX_ROWS` (ordenadas con `order`).
fn row_set(conn: &Connection, query: &str, order: &str) -> Result<RowSet> {
    let count: i64 = conn
        .query_row(&format!("SELECT COUNT(*) FROM ({query})"), [], |row| {
            row.get(0)
        })
        .context("no se pudo comparar la tabla")?;
    if count == 0 {
        return Ok(RowSet::default());
    }

    let mut stmt = conn.prepare(&format!("{query}{order} LIMIT {MAX_ROWS}"))?;
    let width = stmt.column_count();
    let mut rows = stmt.query([])?;
    let mut data = Vec::new();
    while let Some(row) = rows.next()? {
        data.push(
            (0..width)
                .map(|index| row.get::<_, Value>(index))
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
    }
    Ok(RowSet {
        count: count.max(0) as u64,
        rows: data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> Value {
        Value::Text(value.to_string())
    }

    /// Crea la BD actual en un directorio temporal, fija la línea base, aplica `changes` y
    /// compara.
    fn diff_after(setup: &str, changes: &str) -> DatabaseDiff {
        let directory = tempfile::tempdir().unwrap();
        let current = directory.path().join("app.db");
        let baseline = directory.path().join("baseline").join("app.db");
        Connection::open(&current)
            .unwrap()
            .execute_batch(setup)
            .unwrap();
        snapshot(&current, &baseline).unwrap();
        Connection::open(&current)
            .unwrap()
            .execute_batch(changes)
            .unwrap();
        compare(&current, &baseline).unwrap()
    }

    const ITEMS: &str = "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT, qty INTEGER);
        INSERT INTO items VALUES (1, 'teclado', 3), (2, 'mouse', NULL), (3, 'monitor', 1);
        CREATE TABLE untouched (v TEXT); INSERT INTO untouched VALUES ('x');";

    #[test]
    fn reports_inserted_deleted_and_updated_rows() {
        let diff = diff_after(
            ITEMS,
            "INSERT INTO items VALUES (4, 'cable', 10);
             DELETE FROM items WHERE id = 3;
             UPDATE items SET qty = 5 WHERE id = 2;",
        );
        assert_eq!(diff.tables.len(), 1, "las tablas sin cambios no aparecen");
        let items = &diff.tables[0];
        assert_eq!(items.change, TableChange::Modified);
        assert_eq!(items.columns, vec!["id", "name", "qty"]);
        assert_eq!(
            items.inserted.rows,
            vec![vec![Value::Integer(4), text("cable"), Value::Integer(10)]]
        );
        assert_eq!(
            items.deleted.rows,
            vec![vec![Value::Integer(3), text("monitor"), Value::Integer(1)]]
        );
        assert_eq!(
            items.updated.rows,
            vec![UpdatedRow {
                before: vec![Value::Integer(2), text("mouse"), Value::Null],
                after: vec![Value::Integer(2), text("mouse"), Value::Integer(5)],
            }]
        );
        assert_eq!(diff.totals(), (1, 1, 1));
    }

    #[test]
    fn no_changes_means_empty_diff() {
        let diff = diff_after(ITEMS, "UPDATE items SET qty = qty WHERE id = 1;");
        assert!(diff.tables.is_empty());
    }

    #[test]
    fn detects_new_and_dropped_tables() {
        let diff = diff_after(
            ITEMS,
            "CREATE TABLE outbox (id INTEGER PRIMARY KEY, payload TEXT);
             INSERT INTO outbox (payload) VALUES ('{}');
             DROP TABLE untouched;",
        );
        let names: Vec<_> = diff
            .tables
            .iter()
            .map(|table| (table.name.as_str(), table.change))
            .collect();
        assert_eq!(
            names,
            vec![
                ("outbox", TableChange::Added),
                ("untouched", TableChange::Removed)
            ]
        );
        assert_eq!(diff.tables[0].inserted.count, 1);
        assert_eq!(diff.tables[1].deleted.rows, vec![vec![text("x")]]);
    }

    #[test]
    fn uses_rowid_for_tables_without_primary_key_and_composite_keys() {
        let diff = diff_after(
            "CREATE TABLE log (msg TEXT); INSERT INTO log VALUES ('a'), ('b');
             CREATE TABLE pairs (a TEXT, b TEXT, v INTEGER, PRIMARY KEY (a, b)) WITHOUT ROWID;
             INSERT INTO pairs VALUES ('x', 'y', 1), ('x', 'z', 2);",
            "UPDATE log SET msg = 'B' WHERE msg = 'b';
             UPDATE pairs SET v = 20 WHERE a = 'x' AND b = 'z';",
        );
        assert_eq!(diff.tables.len(), 2);
        for table in &diff.tables {
            assert_eq!(table.updated.count, 1, "{}", table.name);
            assert_eq!(table.inserted.count + table.deleted.count, 0);
        }
    }

    #[test]
    fn schema_change_compares_common_columns() {
        let diff = diff_after(ITEMS, "ALTER TABLE items ADD COLUMN sku TEXT;");
        let items = &diff.tables[0];
        assert!(items.schema_changed);
        assert_eq!(items.columns, vec!["id", "name", "qty"]);
        assert_eq!(items.updated.count, 0);
    }

    #[test]
    fn snapshot_includes_uncheckpointed_wal_changes() {
        let directory = tempfile::tempdir().unwrap();
        let current = directory.path().join("app.db");
        let baseline = directory.path().join("baseline").join("app.db");
        let writer = Connection::open(&current).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;
                 CREATE TABLE t (v INTEGER); INSERT INTO t VALUES (1);",
            )
            .unwrap();
        snapshot(&current, &baseline).unwrap();
        writer.execute("INSERT INTO t VALUES (2)", []).unwrap();

        let diff = compare(&current, &baseline).unwrap();
        assert_eq!(diff.tables[0].inserted.rows, vec![vec![Value::Integer(2)]]);
    }
}
