use crate::db::{self, DbObject, ObjectKind, QueryResult};
use crate::diff::{self, DatabaseDiff, TableChange, TableDiff};
use crate::favorites;
use crate::mobile::{self, Device};
use crate::model::{FavoriteQuery, RecentSource, Settings, SourceMode};
use crate::sql_highlight;
use crate::tasks::{TaskKind, Tasks};
use crate::theme;
use eframe::egui::{self, Color32, CornerRadius, Frame, Margin, RichText, Stroke};
use rusqlite::types::Value;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const DEFAULT_SQL: &str =
    "SELECT name, type FROM sqlite_master WHERE type IN ('table', 'view') ORDER BY name;";
const LIVE_WATCH_INTERVAL: Duration = Duration::from_secs(2);
const MAX_RECENTS: usize = 5;

struct CellEdit {
    row: usize,
    column: usize,
    buffer: String,
    focus_pending: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum ResultsTab {
    Data,
    Schema,
}

/// Copia fijada de la BD conectada con la que se comparan los cambios posteriores.
struct Baseline {
    /// BD de la que se tomó: al conectar otra, la línea base deja de valer.
    source: PathBuf,
    copy: PathBuf,
    taken_at: Instant,
}

/// Base de datos de una app en un dispositivo concreto: de dónde se extrajo la copia local y,
/// por tanto, adónde va su push aunque después cambien los selectores.
#[derive(Clone, PartialEq)]
struct MobileTarget {
    mode: SourceMode,
    device: String,
    app_id: String,
    db_name: String,
}

/// Resultado de una operación lanzada en segundo plano. Lleva el contexto con el que se lanzó
/// para descartar respuestas que ya no corresponden a lo que hay en pantalla.
enum TaskOutput {
    Devices {
        mode: SourceMode,
        /// Se pasa a la detección de apps que sigue a esta.
        fallback: bool,
        result: anyhow::Result<Vec<Device>>,
    },
    Apps {
        mode: SourceMode,
        device: String,
        /// Si no aparece ninguna app, buscar igualmente las bases de datos del identificador
        /// escrito a mano.
        fallback: bool,
        result: anyhow::Result<Vec<String>>,
    },
    Databases {
        mode: SourceMode,
        device: String,
        app_id: String,
        result: anyhow::Result<Vec<String>>,
    },
    Pulled {
        target: MobileTarget,
        result: anyhow::Result<PathBuf>,
    },
    Pushed {
        revision: u64,
        message: Option<String>,
        result: anyhow::Result<()>,
    },
}

pub struct DbBridgeApp {
    settings: Settings,
    detected_devices: Vec<Device>,
    detected_apps: Vec<String>,
    detected_dbs: Vec<String>,
    db_path: Option<PathBuf>,
    /// Origen de la copia local cuando se extrajo de un dispositivo.
    target: Option<MobileTarget>,
    origin: Option<String>,
    objects: Vec<DbObject>,
    selected: Option<DbObject>,
    table_filter: String,
    sql: String,
    generated_sql: Option<String>,
    /// Página de la tabla seleccionada (de `db::ROW_LIMIT` filas) mientras se explora.
    page: usize,
    result: QueryResult,
    results_tab: ResultsTab,
    /// Esquema de la tabla seleccionada, cargado al abrir la pestaña y guardado con su nombre.
    schema: Option<(String, Result<db::TableSchema, String>)>,
    editing: Option<CellEdit>,
    live_watch: bool,
    last_refresh: Instant,
    dirty: bool,
    /// Se incrementa con cada cambio local; un push solo limpia `dirty` si no hubo cambios
    /// posteriores a la copia que envió.
    change_revision: u64,
    /// Mensaje del último cambio que llegó con un push ya en curso; se envía al terminar este.
    queued_push: Option<String>,
    tasks: Tasks<TaskOutput>,
    baseline: Option<Baseline>,
    /// Comparación mostrada en lugar del editor; se recalcula en cada reconexión.
    diff: Option<DatabaseDiff>,
    status: String,
    new_favorite_name: String,
}

impl DbBridgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);

        Self {
            settings: favorites::load(),
            detected_devices: Vec::new(),
            detected_apps: Vec::new(),
            detected_dbs: Vec::new(),
            db_path: None,
            target: None,
            origin: None,
            objects: Vec::new(),
            selected: None,
            table_filter: String::new(),
            sql: DEFAULT_SQL.to_string(),
            generated_sql: None,
            page: 0,
            result: QueryResult::default(),
            results_tab: ResultsTab::Data,
            schema: None,
            editing: None,
            live_watch: false,
            last_refresh: Instant::now(),
            dirty: false,
            change_revision: 0,
            queued_push: None,
            tasks: Tasks::new(cc.egui_ctx.clone()),
            baseline: None,
            diff: None,
            status: "Abre un archivo o extrae la base de datos de un emulador.".to_string(),
            new_favorite_name: String::new(),
        }
    }

    fn persist(&mut self) {
        if let Err(error) = favorites::save(&self.settings) {
            self.status = format!("No se pudieron guardar los ajustes: {error:#}");
        }
    }

    /// Lista los dispositivos y, si hay alguno, sigue con sus apps y sus bases de datos.
    fn detect_devices(&mut self, fallback: bool) {
        let mode = self.settings.mode;
        if mode == SourceMode::Local {
            return;
        }
        self.tasks
            .spawn(TaskKind::DetectDevices, move || TaskOutput::Devices {
                mode,
                fallback,
                result: mobile::detect_devices(mode),
            });
    }

    fn finish_detect_devices(
        &mut self,
        mode: SourceMode,
        fallback: bool,
        result: anyhow::Result<Vec<Device>>,
    ) {
        if mode != self.settings.mode {
            return;
        }
        match result {
            Ok(devices) if devices.is_empty() => {
                self.detected_devices.clear();
                self.detected_apps.clear();
                self.detected_dbs.clear();
                self.status = match mode {
                    SourceMode::Ios => "No hay ningún simulador de iOS encendido.",
                    _ => "No hay ningún dispositivo o emulador conectado a adb.",
                }
                .to_string();
            }
            Ok(devices) => {
                // Se conserva el elegido si sigue conectado; si no, el primero de la lista.
                if !devices
                    .iter()
                    .any(|device| device.id == self.settings.device)
                {
                    self.settings.device = devices[0].id.clone();
                    self.detected_apps.clear();
                    self.detected_dbs.clear();
                    self.persist();
                }
                self.detected_devices = devices;
                self.detect_apps(fallback);
            }
            Err(error) => {
                self.detected_devices.clear();
                self.status = format!("No se pudieron listar los dispositivos: {error:#}");
            }
        }
    }

    fn select_device(&mut self, device: String) {
        if device == self.settings.device {
            return;
        }
        self.settings.device = device;
        self.detected_apps.clear();
        self.detected_dbs.clear();
        self.persist();
        self.detect_apps(true);
    }

    fn detect_apps(&mut self, fallback: bool) {
        let mode = self.settings.mode;
        if mode == SourceMode::Local {
            return;
        }
        let device = self.settings.device.clone();
        self.tasks.spawn(TaskKind::DetectApps, move || {
            let result = mobile::detect_apps(mode, &device);
            TaskOutput::Apps {
                mode,
                device,
                fallback,
                result,
            }
        });
    }

    fn finish_detect_apps(
        &mut self,
        mode: SourceMode,
        device: String,
        fallback: bool,
        result: anyhow::Result<Vec<String>>,
    ) {
        if mode != self.settings.mode || device != self.settings.device {
            return;
        }
        let mut found_apps = false;
        match result {
            Ok(apps) => {
                if apps.is_empty() {
                    self.status = "No se encontraron apps de usuario instaladas en el dispositivo."
                        .to_string();
                } else {
                    if self.settings.app_id.trim().is_empty()
                        || !apps.iter().any(|app| app == self.settings.app_id.trim())
                    {
                        self.settings.app_id = apps[0].clone();
                    }
                    self.status = format!("{} aplicaciones detectadas.", apps.len());
                }
                found_apps = !apps.is_empty();
                self.detected_apps = apps;
            }
            Err(error) => {
                self.status = format!("No se pudieron detectar apps: {error:#}");
            }
        }
        if found_apps || (fallback && self.detected_apps.is_empty()) {
            self.detect_databases();
        }
    }

    fn detect_databases(&mut self) {
        let mode = self.settings.mode;
        let device = self.settings.device.clone();
        let app_id = self.settings.app_id.trim().to_string();
        if mode == SourceMode::Local || app_id.is_empty() {
            self.detected_dbs.clear();
            return;
        }
        self.tasks.spawn(TaskKind::DetectDatabases, move || {
            let result = mobile::detect_databases(mode, &device, &app_id);
            TaskOutput::Databases {
                mode,
                device,
                app_id,
                result,
            }
        });
    }

    fn finish_detect_databases(
        &mut self,
        mode: SourceMode,
        device: String,
        app_id: String,
        result: anyhow::Result<Vec<String>>,
    ) {
        // El usuario cambió de modo, de dispositivo o de app mientras se buscaba.
        if mode != self.settings.mode
            || device != self.settings.device
            || app_id != self.settings.app_id.trim()
        {
            return;
        }
        match result {
            Ok(databases) => {
                if databases.is_empty() {
                    self.status = format!(
                        "No se encontraron bases de datos en {app_id}. Escribe el nombre del archivo."
                    );
                } else {
                    // Un nombre guardado de antes («offline.db») se traduce a su ruta detectada.
                    let current = self.settings.db_name.trim();
                    let matching = databases
                        .iter()
                        .find(|path| {
                            path.as_str() == current || path.ends_with(&format!("/{current}"))
                        })
                        .cloned();
                    self.settings.db_name = matching.unwrap_or_else(|| databases[0].clone());
                    self.status = format!(
                        "{} bases de datos encontradas en {app_id}.",
                        databases.len()
                    );
                }
                self.detected_dbs = databases;
            }
            Err(error) => {
                self.detected_dbs.clear();
                self.status = format!("No se pudieron detectar bases de datos: {error:#}");
            }
        }
    }

    fn pick_local_file(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("SQLite", &["db", "sqlite", "sqlite3"])
            .pick_file()
        {
            self.open_local(path);
        }
    }

    fn open_local(&mut self, path: PathBuf) {
        self.settings.mode = SourceMode::Local;
        self.detected_devices.clear();
        self.detected_apps.clear();
        self.detected_dbs.clear();
        let origin = file_name(&path);
        if self.connect(path.clone(), origin) {
            self.target = None;
            self.remember_recent(RecentSource {
                mode: SourceMode::Local,
                device: String::new(),
                app_id: String::new(),
                db_name: String::new(),
                path: Some(path),
            });
        }
        self.persist();
    }

    fn open_recent(&mut self, recent: RecentSource) {
        match recent.path {
            Some(path) => {
                if path.exists() {
                    self.open_local(path);
                } else {
                    self.status = format!("No se encontró {}", path.display());
                }
            }
            None => {
                self.settings.mode = recent.mode;
                // Los recientes guardados antes del selector no tienen dispositivo.
                if !recent.device.is_empty() {
                    self.settings.device = recent.device;
                }
                self.settings.app_id = recent.app_id;
                self.settings.db_name = recent.db_name;
                self.detected_devices.clear();
                self.detected_apps.clear();
                self.detected_dbs.clear();
                self.pull_selected();
            }
        }
    }

    fn remember_recent(&mut self, recent: RecentSource) {
        self.settings.recents.retain(|existing| *existing != recent);
        self.settings.recents.insert(0, recent);
        self.settings.recents.truncate(MAX_RECENTS);
    }

    /// Pull y push leen y escriben la copia local y el dispositivo: nunca dos a la vez.
    fn transfer_running(&self) -> bool {
        self.tasks.is_running(TaskKind::Pull) || self.tasks.is_running(TaskKind::Push)
    }

    /// Extrae la base de datos elegida en los selectores.
    fn pull_selected(&mut self) {
        let mode = self.settings.mode;
        if mode == SourceMode::Local {
            return;
        }
        let app_id = self.settings.app_id.trim().to_string();
        let db_name = self.settings.db_name.trim().to_string();
        if app_id.is_empty() || db_name.is_empty() {
            self.status =
                "Indica el identificador de la app y el nombre del archivo de base de datos."
                    .to_string();
            return;
        }
        self.pull_mobile(MobileTarget {
            mode,
            device: self.settings.device.clone(),
            app_id,
            db_name,
        });
    }

    fn pull_mobile(&mut self, target: MobileTarget) {
        if self.transfer_running() {
            self.status = "Espera a que termine la transferencia en curso.".to_string();
            return;
        }
        self.tasks.spawn(TaskKind::Pull, move || {
            let result = mobile::pull(target.mode, &target.device, &target.app_id, &target.db_name);
            TaskOutput::Pulled { target, result }
        });
    }

    fn finish_pull(&mut self, target: MobileTarget, result: anyhow::Result<PathBuf>) {
        // Se pasó a archivo local u otro modo mientras se extraía.
        if target.mode != self.settings.mode {
            return;
        }
        match result {
            Ok(path) => {
                let origin = format!("{}/{}", target.app_id, target.db_name);
                if self.connect(path, origin) {
                    self.remember_recent(RecentSource {
                        mode: target.mode,
                        device: target.device.clone(),
                        app_id: target.app_id.clone(),
                        db_name: target.db_name.clone(),
                        path: None,
                    });
                    self.target = Some(target);
                }
                self.persist();
            }
            Err(error) => {
                self.status = format!("Error extrayendo la base de datos: {error:#}");
            }
        }
    }

    /// Envía la copia local al dispositivo en segundo plano. `message` describe el cambio que
    /// lo motivó, si lo hay, para el texto de la barra de estado.
    fn push_mobile(&mut self, message: Option<String>) {
        let (Some(local), Some(target)) = (self.db_path.clone(), self.target.clone()) else {
            return;
        };
        if self.tasks.is_running(TaskKind::Pull) {
            self.status = "Espera a que termine la extracción en curso.".to_string();
            return;
        }
        if self.tasks.is_running(TaskKind::Push) {
            // El push siguiente lleva todos los cambios acumulados: basta con recordar el último.
            let message = message.unwrap_or_default();
            self.status = format!("{message} En cola para enviar al dispositivo.")
                .trim_start()
                .to_string();
            self.queued_push = Some(message);
            return;
        }

        if let Err(error) = db::checkpoint(&local) {
            self.status = format!("Error preparando la base de datos para el push: {error:#}");
            return;
        }
        let snapshot = match mobile::push_snapshot(&local) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.status = format!("Error preparando la base de datos para el push: {error:#}");
                return;
            }
        };

        let revision = self.change_revision;
        self.status = match &message {
            Some(message) => format!("{message} Enviando al dispositivo…"),
            None => "Enviando al dispositivo…".to_string(),
        };
        self.tasks
            .spawn(TaskKind::Push, move || TaskOutput::Pushed {
                revision,
                message,
                result: mobile::push(
                    target.mode,
                    &target.device,
                    &target.app_id,
                    &target.db_name,
                    &snapshot,
                ),
            });
    }

    fn finish_push(&mut self, revision: u64, message: Option<String>, result: anyhow::Result<()>) {
        match result {
            Ok(()) => {
                if revision == self.change_revision {
                    self.dirty = false;
                }
                self.status = match message {
                    Some(message) => {
                        format!("{message} Enviado al dispositivo: reabre la app para verlo.")
                    }
                    None => "Base de datos enviada. Reinicia la app para que vuelva a abrirla."
                        .to_string(),
                };
            }
            // El cambio sigue pendiente («Cambios sin push») y el error queda en la barra.
            Err(error) => self.status = format!("Error enviando la base de datos: {error:#}"),
        }
        if let Some(message) = self.queued_push.take() {
            self.push_mobile((!message.is_empty()).then_some(message));
        }
    }

    fn poll_tasks(&mut self) {
        for (kind, output) in self.tasks.finished() {
            match output {
                Some(TaskOutput::Devices {
                    mode,
                    fallback,
                    result,
                }) => self.finish_detect_devices(mode, fallback, result),
                Some(TaskOutput::Apps {
                    mode,
                    device,
                    fallback,
                    result,
                }) => self.finish_detect_apps(mode, device, fallback, result),
                Some(TaskOutput::Databases {
                    mode,
                    device,
                    app_id,
                    result,
                }) => self.finish_detect_databases(mode, device, app_id, result),
                Some(TaskOutput::Pulled { target, result }) => self.finish_pull(target, result),
                Some(TaskOutput::Pushed {
                    revision,
                    message,
                    result,
                }) => self.finish_push(revision, message, result),
                None => {
                    self.status = format!("Error inesperado: «{}» se interrumpió.", kind.label());
                }
            }
        }
    }

    /// Registra un cambio ya aplicado en la copia local. Con un archivo local el cambio ya está
    /// en el origen; con un dispositivo se envía al momento si el push automático está activo.
    fn record_change(&mut self, message: String) {
        // Puede ser DDL (ALTER, CREATE INDEX…): el esquema se vuelve a leer al mostrarlo.
        self.schema = None;
        if self.target.is_none() || self.db_path.is_none() {
            self.status = message;
            return;
        }
        self.dirty = true;
        self.change_revision += 1;
        if self.settings.auto_push {
            self.push_mobile(Some(message));
        } else {
            self.status = format!("{message} Pulsa «Push al emulador» para enviarlo.");
        }
    }

    fn connect(&mut self, path: PathBuf, origin: String) -> bool {
        let conn = match db::open(&path) {
            Ok(conn) => conn,
            Err(error) => {
                self.status = format!("Error abriendo SQLite: {error:#}");
                return false;
            }
        };

        let objects = match db::list_objects(&conn) {
            Ok(objects) => objects,
            Err(error) => {
                self.status = format!("Error leyendo el esquema: {error:#}");
                return false;
            }
        };

        self.db_path = Some(path);
        self.objects = objects;
        self.editing = None;
        self.dirty = false;
        self.schema = None;
        // Un push en cola era para la copia anterior.
        self.queued_push = None;
        if self.sql.trim().is_empty() {
            self.sql = DEFAULT_SQL.to_string();
            self.generated_sql = None;
        }
        self.status = format!("Conectado: {origin}");
        self.origin = Some(origin);
        if self
            .baseline
            .as_ref()
            .is_some_and(|baseline| Some(&baseline.source) != self.db_path.as_ref())
        {
            self.baseline = None;
            self.diff = None;
        } else if self.diff.is_some() {
            // Live Watch o Refrescar: la vista de cambios se mantiene al día.
            self.compare_with_baseline();
        }
        // Reconectar (Live Watch, Refrescar) no debe repetir un DELETE o UPDATE del editor.
        if db::is_read_only(&conn, &self.sql) {
            self.run_sql();
        } else {
            self.result = QueryResult::default();
        }
        true
    }

    fn set_baseline(&mut self) {
        let Some(source) = self.db_path.clone() else {
            return;
        };
        if self.tasks.is_running(TaskKind::Pull) {
            self.status = "Espera a que termine la extracción en curso.".to_string();
            return;
        }
        let copy = std::env::temp_dir()
            .join("db-bridge")
            .join("baseline")
            .join(std::process::id().to_string())
            .join(source.file_name().unwrap_or_default());
        match diff::snapshot(&source, &copy) {
            Ok(()) => {
                self.baseline = Some(Baseline {
                    source,
                    copy,
                    taken_at: Instant::now(),
                });
                self.status = if self.target.is_some() {
                    "Línea base fijada. Usa la app y pulsa Refrescar (o activa Live Watch) para ver los cambios."
                } else {
                    "Línea base fijada. Los cambios en el archivo aparecerán en «Ver cambios»."
                }
                .to_string();
                if self.diff.is_some() {
                    self.compare_with_baseline();
                }
            }
            Err(error) => self.status = format!("No se pudo fijar la línea base: {error:#}"),
        }
    }

    fn compare_with_baseline(&mut self) {
        let (Some(current), Some(baseline)) = (&self.db_path, &self.baseline) else {
            return;
        };
        match diff::compare(current, &baseline.copy) {
            Ok(result) => {
                self.editing = None;
                self.diff = Some(result);
            }
            Err(error) => {
                self.diff = None;
                self.status = format!("No se pudo comparar con la línea base: {error:#}");
            }
        }
    }

    fn clear_baseline(&mut self) {
        self.baseline = None;
        self.diff = None;
        self.status = "Línea base descartada.".to_string();
    }

    fn select_object(&mut self, object: DbObject) {
        self.selected = Some(object);
        self.diff = None;
        self.go_to_page(0);
    }

    fn go_to_page(&mut self, page: usize) {
        let Some(object) = &self.selected else {
            return;
        };
        self.page = page;
        self.sql = db::table_page_sql(object, page);
        self.generated_sql = Some(self.sql.clone());
        self.editing = None;
        self.run_sql();
    }

    fn refresh(&mut self) {
        if self.db_path.is_none() {
            self.status = "No hay ninguna base de datos conectada.".to_string();
            return;
        }
        match self.target.clone() {
            Some(target) => self.pull_mobile(target),
            None => {
                let read_only = self
                    .db_path
                    .as_ref()
                    .and_then(|path| db::open(path).ok())
                    .map(|conn| db::is_read_only(&conn, &self.sql))
                    .unwrap_or(true);
                if read_only {
                    self.run_sql();
                } else {
                    self.status =
                        "La consulta del editor modifica datos: pulsa Ejecutar para repetirla."
                            .to_string();
                }
            }
        }
    }

    fn run_sql(&mut self) {
        let Some(path) = self.db_path.clone() else {
            self.status = "No hay ninguna base de datos conectada.".to_string();
            return;
        };
        // La copia local se está sobrescribiendo: se ejecutará al conectar si es de lectura.
        if self.tasks.is_running(TaskKind::Pull) {
            self.status = "Espera a que termine la extracción en curso.".to_string();
            return;
        }

        let conn = match db::open(&path) {
            Ok(conn) => conn,
            Err(error) => {
                self.status = format!("Error abriendo la base de datos: {error:#}");
                return;
            }
        };

        let is_generated = self
            .generated_sql
            .as_ref()
            .map(|generated| generated.trim() == self.sql.trim())
            .unwrap_or(false);

        let outcome = if is_generated {
            self.selected
                .as_ref()
                .map(|object| {
                    db::run_table_query(&conn, object, db::ROW_LIMIT, self.page * db::ROW_LIMIT)
                })
                .unwrap_or_else(|| db::run_query(&conn, &self.sql))
        } else {
            db::run_query(&conn, &self.sql)
        };

        match outcome {
            Ok(result) => {
                // La tabla encogió (Live Watch, Refrescar) y la página ya no existe.
                if let Some(total) = result.total {
                    let last_page = last_page(total);
                    if self.page > last_page {
                        self.go_to_page(last_page);
                        return;
                    }
                }
                let rows = result.rows.len();
                let elapsed = result.elapsed.as_millis();
                let modified = result.modified;
                self.result = result;
                if modified {
                    self.record_change(format!("Sentencia ejecutada ({elapsed} ms)."));
                } else {
                    self.status = format!("{rows} filas · {elapsed} ms");
                }
            }
            Err(error) => {
                self.result = QueryResult::default();
                self.status = format!("Error SQL: {error:#}");
            }
        }
    }

    fn apply_edit(&mut self, row: usize, column: usize, new_text: String) {
        let Some(path) = self.db_path.clone() else {
            return;
        };
        if self.tasks.is_running(TaskKind::Pull) {
            self.status =
                "Edición descartada: la base de datos se está volviendo a extraer.".to_string();
            return;
        }
        let Some(object) = self.selected.clone() else {
            self.status = "La vista actual no permite editar celdas.".to_string();
            return;
        };
        let Some(key_expr) = self.result.key_expr.clone() else {
            self.status = "La vista actual no permite editar celdas.".to_string();
            return;
        };
        let Some(key) = self.result.keys.get(row).cloned() else {
            self.status = "No se encontró la clave de la fila.".to_string();
            return;
        };
        let Some(column_name) = self.result.columns.get(column).cloned() else {
            self.status = "No se encontró la columna.".to_string();
            return;
        };

        let new_value = if new_text.eq_ignore_ascii_case("NULL") {
            Value::Null
        } else {
            Value::Text(new_text.clone())
        };

        let conn = match db::open(&path) {
            Ok(conn) => conn,
            Err(error) => {
                self.status = format!("Error abriendo la base de datos: {error:#}");
                return;
            }
        };

        match db::update_cell(
            &conn,
            &object.name,
            &key_expr,
            &key,
            &column_name,
            &new_value,
        ) {
            Ok(1) => {
                if let Some(value) = self
                    .result
                    .rows
                    .get_mut(row)
                    .and_then(|row| row.get_mut(column))
                {
                    *value = new_value;
                }
                self.record_change(format!("«{column_name}» actualizado (fila {}).", row + 1));
            }
            Ok(_) => {
                self.status = "La actualización no afectó ninguna fila.".to_string();
            }
            Err(error) => {
                self.status = format!("Error actualizando la celda: {error:#}");
            }
        }
    }

    fn export_csv(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("export.csv")
            .add_filter("CSV", &["csv"])
            .save_file()
        else {
            return;
        };
        let content = db::to_csv(&self.result);
        match std::fs::write(&path, content) {
            Ok(()) => self.status = format!("CSV exportado en {}", path.display()),
            Err(error) => self.status = format!("No se pudo exportar el CSV: {error:#}"),
        }
    }

    fn export_json(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("export.json")
            .add_filter("JSON", &["json"])
            .save_file()
        else {
            return;
        };
        let content = match serde_json::to_string_pretty(&db::to_json(&self.result)) {
            Ok(content) => content,
            Err(error) => {
                self.status = format!("No se pudo generar el JSON: {error:#}");
                return;
            }
        };
        match std::fs::write(&path, content) {
            Ok(()) => self.status = format!("JSON exportado en {}", path.display()),
            Err(error) => self.status = format!("No se pudo exportar el JSON: {error:#}"),
        }
    }

    fn save_favorite(&mut self) {
        let name = self.new_favorite_name.trim().to_string();
        if name.is_empty() || self.sql.trim().is_empty() {
            self.status = "Escribe un nombre y una consulta antes de guardarla.".to_string();
            return;
        }
        self.settings.favorites.push(FavoriteQuery {
            name,
            sql: self.sql.clone(),
        });
        self.new_favorite_name.clear();
        self.persist();
        self.status = "Consulta guardada en favoritos.".to_string();
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .filter(|path| !path.as_os_str().is_empty())
                .collect::<Vec<_>>()
        });
        if let Some(path) = dropped.into_iter().next() {
            self.open_local(path);
        }
    }

    fn tick_live_watch(&mut self, ctx: &egui::Context) {
        if !self.live_watch {
            return;
        }
        if self.last_refresh.elapsed() >= LIVE_WATCH_INTERVAL
            && self.editing.is_none()
            && !self.dirty
            && self.db_path.is_some()
            && !self.transfer_running()
        {
            self.last_refresh = Instant::now();
            self.refresh();
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn status_color(&self) -> Color32 {
        let status = self.status.as_str();
        if status.starts_with("Error") || status.starts_with("No se pudo") {
            theme::DANGER
        } else if self.db_path.is_some() {
            theme::SUCCESS
        } else {
            theme::FAINT
        }
    }

    fn top_panel(&mut self, ui: &mut egui::Ui) {
        let frame = Frame::NONE
            .fill(theme::SURFACE)
            .inner_margin(Margin::symmetric(16, 10));
        egui::Panel::top("top_panel").frame(frame).show(ui, |ui| {
            ui.horizontal(|ui| {
                theme::dot(ui, theme::ACCENT, 5.0);
                ui.label(theme::strong("DB Bridge").size(14.5));
                ui.add_space(12.0);

                let modes = [SourceMode::Local, SourceMode::Ios, SourceMode::Android]
                    .map(|mode| (mode, mode.label()));
                if theme::segmented(ui, &mut self.settings.mode, &modes) {
                    self.detected_devices.clear();
                    self.detected_apps.clear();
                    self.detected_dbs.clear();
                    self.editing = None;
                    if self.settings.mode != SourceMode::Local {
                        self.detect_devices(false);
                    }
                    self.persist();
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let transfer_idle = !self.transfer_running();
                    if self.db_path.is_some()
                        && self.target.is_some()
                        && ui
                            .add_enabled(
                                transfer_idle,
                                theme::warning_button("↑  Push al emulador"),
                            )
                            .on_hover_text(
                                "Cierra la app y copia la base de datos editada al dispositivo.",
                            )
                            .clicked()
                    {
                        self.push_mobile(None);
                    }
                    if self.settings.mode != SourceMode::Local
                        && ui
                            .checkbox(&mut self.settings.auto_push, "Push automático")
                            .on_hover_text(
                                "Envía cada cambio al dispositivo en cuanto se aplica. \
                                 Cada envío cierra la app para copiar la base de datos.",
                            )
                            .changed()
                    {
                        self.persist();
                    }
                    ui.checkbox(&mut self.live_watch, "Live Watch · 2 s")
                        .on_hover_text("Vuelve a extraer y ejecutar la consulta cada 2 segundos.");
                });
            });

            ui.add_space(8.0);

            ui.horizontal_wrapped(|ui| match self.settings.mode {
                SourceMode::Local => {
                    if ui
                        .add(theme::primary_button("Abrir archivo SQLite…"))
                        .clicked()
                    {
                        self.pick_local_file();
                    }
                    ui.weak("o arrastra un archivo .db a la ventana");
                }
                SourceMode::Ios | SourceMode::Android => {
                    let mut chosen_device = None;
                    if !self.detected_devices.is_empty() {
                        theme::field(ui, "Dispositivo", |ui| {
                            let selected = self
                                .detected_devices
                                .iter()
                                .find(|device| device.id == self.settings.device)
                                .map_or(self.settings.device.as_str(), |device| {
                                    device.name.as_str()
                                });
                            egui::ComboBox::from_id_salt("device_selector")
                                .selected_text(selected)
                                .width(200.0)
                                .truncate()
                                .show_ui(ui, |ui| {
                                    for device in &self.detected_devices {
                                        let current = device.id == self.settings.device;
                                        if ui
                                            .selectable_label(current, device.name.as_str())
                                            .on_hover_text(theme::mono(device.id.as_str()))
                                            .clicked()
                                        {
                                            chosen_device = Some(device.id.clone());
                                        }
                                    }
                                })
                                .response
                                .on_hover_text(self.settings.device.as_str());
                        });
                    }
                    if let Some(device) = chosen_device {
                        self.select_device(device);
                    }

                    let label = if self.settings.mode == SourceMode::Ios {
                        "Bundle ID"
                    } else {
                        "Package"
                    };
                    let mut app_changed = false;
                    theme::field(ui, label, |ui| {
                        if self.detected_apps.is_empty() {
                            let response = ui.add(
                                egui::TextEdit::singleline(&mut self.settings.app_id)
                                    .frame(Frame::NONE)
                                    .font(egui::TextStyle::Monospace)
                                    .desired_width(220.0)
                                    .align(egui::Align2::LEFT_CENTER)
                                    .hint_text("com.miempresa.app"),
                            );
                            if response.changed() {
                                self.detected_dbs.clear();
                            }
                        } else {
                            egui::ComboBox::from_id_salt("app_selector")
                                .selected_text(if self.settings.app_id.is_empty() {
                                    RichText::new("Selecciona una app").color(theme::SUBTLE)
                                } else {
                                    theme::mono(self.settings.app_id.as_str())
                                })
                                .width(240.0)
                                .show_ui(ui, |ui| {
                                    for app in &self.detected_apps {
                                        if ui
                                            .selectable_value(
                                                &mut self.settings.app_id,
                                                app.clone(),
                                                theme::mono(app),
                                            )
                                            .changed()
                                        {
                                            app_changed = true;
                                        }
                                    }
                                });
                        }
                    });
                    if app_changed {
                        self.detect_databases();
                    }

                    theme::field(ui, "Archivo", |ui| {
                        if self.detected_dbs.is_empty() {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.settings.db_name)
                                    .frame(Frame::NONE)
                                    .font(egui::TextStyle::Monospace)
                                    .desired_width(260.0)
                                    .align(egui::Align2::LEFT_CENTER)
                                    .hint_text("offline.db"),
                            );
                        } else {
                            egui::ComboBox::from_id_salt("db_selector")
                                .selected_text(theme::mono(self.settings.db_name.as_str()))
                                .width(260.0)
                                .truncate()
                                .show_ui(ui, |ui| {
                                    for database in &self.detected_dbs {
                                        ui.selectable_value(
                                            &mut self.settings.db_name,
                                            database.clone(),
                                            theme::mono(database),
                                        );
                                    }
                                })
                                .response
                                .on_hover_text(self.settings.db_name.as_str());
                        }
                    });

                    let detecting = self.tasks.is_running(TaskKind::DetectDevices)
                        || self.tasks.is_running(TaskKind::DetectApps)
                        || self.tasks.is_running(TaskKind::DetectDatabases);
                    if ui
                        .add_enabled(!detecting, egui::Button::new("Detectar"))
                        .on_hover_text(
                            "Busca los dispositivos, sus apps instaladas y las bases de datos \
                             de la app elegida.",
                        )
                        .clicked()
                    {
                        // Con un identificador escrito a mano no hay lista de apps, pero sí se
                        // pueden buscar sus bases de datos.
                        self.detect_devices(true);
                    }
                    if ui
                        .add_enabled(
                            !self.transfer_running(),
                            theme::primary_button("Extraer y conectar"),
                        )
                        .clicked()
                    {
                        self.pull_selected();
                    }
                }
            });
        });
    }

    fn status_panel(&mut self, ui: &mut egui::Ui) {
        let frame = Frame::NONE
            .fill(theme::SURFACE)
            .inner_margin(Margin::symmetric(16, 5));
        egui::Panel::bottom("status_panel")
            .frame(frame)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(kind) = self.tasks.running().last() {
                        ui.add(egui::Spinner::new().size(12.0).color(theme::ACCENT));
                        ui.label(RichText::new(kind.label()).size(12.0).color(theme::TEXT));
                        ui.add_space(8.0);
                    }
                    theme::dot(ui, self.status_color(), 3.5);
                    ui.add(
                        egui::Label::new(
                            RichText::new(&self.status).size(12.0).color(theme::MUTED),
                        )
                        .truncate(),
                    );
                    if self.dirty {
                        ui.add_space(8.0);
                        theme::dot(ui, theme::WARNING, 3.5);
                        ui.label(
                            RichText::new("Cambios sin push")
                                .size(12.0)
                                .color(theme::WARNING),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(path) = &self.db_path {
                            ui.label(
                                theme::mono(path.display().to_string())
                                    .size(11.5)
                                    .color(theme::SUBTLE),
                            );
                        }
                    });
                });
            });
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        let frame = Frame::NONE
            .fill(theme::SIDEBAR)
            .inner_margin(Margin::symmetric(12, 14));
        egui::Panel::left("side_panel")
            .resizable(true)
            .default_size(260.0)
            .size_range(220.0..=420.0)
            .frame(frame)
            .show(ui, |ui| {
                self.connection_card(ui);
                ui.add_space(16.0);

                ui.horizontal(|ui| {
                    theme::section_label(ui, "Tablas");
                    if !self.objects.is_empty() {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(self.objects.len().to_string())
                                    .size(11.5)
                                    .color(theme::SUBTLE),
                            );
                        });
                    }
                });
                ui.add(
                    egui::TextEdit::singleline(&mut self.table_filter)
                        .hint_text("Filtrar tablas")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(2.0);

                egui::ScrollArea::vertical()
                    .id_salt("objects_scroll")
                    .max_height(320.0)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        if self.objects.is_empty() {
                            ui.weak("Sin conexión.");
                        }
                        let filter = self.table_filter.trim().to_lowercase();
                        for object in self.objects.clone() {
                            if !filter.is_empty() && !object.name.to_lowercase().contains(&filter) {
                                continue;
                            }
                            let selected = self
                                .selected
                                .as_ref()
                                .map(|current| current.name == object.name)
                                .unwrap_or(false);
                            let tag = match (object.kind, object.without_rowid) {
                                (ObjectKind::View, _) => "vista",
                                (ObjectKind::Table, true) => "sin rowid",
                                _ => "",
                            };
                            let name_color = if selected {
                                theme::TEXT
                            } else {
                                theme::TEXT_SOFT
                            };
                            let button = egui::Button::selectable(
                                selected,
                                theme::mono(object.name.as_str()).color(name_color),
                            )
                            .right_text(RichText::new(tag).size(11.0).color(theme::SUBTLE))
                            .min_size(egui::vec2(ui.available_width(), 28.0));
                            let is_view = object.kind == ObjectKind::View;
                            let response = ui.add(button);
                            if response.clicked() {
                                self.select_object(object);
                            }
                            if is_view {
                                response.on_hover_text("Las vistas son de solo lectura.");
                            }
                        }
                    });

                ui.add_space(16.0);
                theme::section_label(ui, "Favoritas");

                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.new_favorite_name)
                            .hint_text("Nombre de la consulta")
                            .desired_width((ui.available_width() - 84.0).max(80.0)),
                    );
                    if ui.button("Guardar").clicked() {
                        self.save_favorite();
                    }
                });
                ui.add_space(2.0);

                egui::ScrollArea::vertical()
                    .id_salt("favorites_scroll")
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let mut remove = None;
                        for (index, favorite) in self.settings.favorites.clone().iter().enumerate()
                        {
                            ui.horizontal(|ui| {
                                let width = (ui.available_width() - 36.0).max(60.0);
                                let button = egui::Button::selectable(
                                    false,
                                    RichText::new(format!("★  {}", favorite.name))
                                        .color(theme::TEXT_SOFT),
                                )
                                .right_text("")
                                .min_size(egui::vec2(width, 28.0));
                                if ui.add(button).on_hover_text(&favorite.sql).clicked() {
                                    self.sql = favorite.sql.clone();
                                    self.generated_sql = None;
                                    self.selected = None;
                                    self.diff = None;
                                    self.editing = None;
                                    self.run_sql();
                                }
                                let delete = egui::Button::new(
                                    RichText::new("×").size(15.0).color(theme::SUBTLE),
                                )
                                .frame_when_inactive(false)
                                .min_size(egui::vec2(28.0, 28.0));
                                if ui.add(delete).on_hover_text("Eliminar").clicked() {
                                    remove = Some(index);
                                }
                            });
                        }
                        if let Some(index) = remove {
                            self.settings.favorites.remove(index);
                            self.persist();
                        }
                    });
            });
    }

    fn connection_card(&mut self, ui: &mut egui::Ui) {
        theme::card().inner_margin(Margin::same(12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            match &self.db_path {
                Some(path) => {
                    ui.horizontal(|ui| {
                        theme::dot(ui, theme::SUCCESS, 4.0);
                        ui.label(theme::strong(self.settings.mode.label()));
                    });
                    if let Some(origin) = &self.origin {
                        ui.add(
                            egui::Label::new(theme::mono(origin.as_str()).color(theme::MUTED))
                                .truncate(),
                        )
                        .on_hover_text(path.display().to_string());
                    }
                    if let Some((main, wal)) = db::disk_size(path) {
                        let mut text = db::format_size(main);
                        let mut detail = format!("{main} bytes");
                        if wal > 0 {
                            text.push_str(&format!(" · WAL {}", db::format_size(wal)));
                            detail.push_str(&format!(" + {wal} bytes de WAL"));
                        }
                        ui.label(RichText::new(text).size(12.0).color(theme::SUBTLE))
                            .on_hover_text(detail);
                    }
                    ui.add_space(6.0);
                    self.baseline_controls(ui);
                }
                None => {
                    ui.horizontal(|ui| {
                        theme::dot(ui, theme::FAINT, 4.0);
                        ui.label(theme::strong("Sin conexión"));
                    });
                    ui.label(
                        RichText::new("Abre un archivo o extrae la BD de un dispositivo.")
                            .size(12.0)
                            .color(theme::SUBTLE),
                    );
                }
            }
        });
    }

    fn baseline_controls(&mut self, ui: &mut egui::Ui) {
        let pulling = self.tasks.is_running(TaskKind::Pull);
        match &self.baseline {
            None => {
                if ui
                    .add_enabled(!pulling, egui::Button::new("Fijar línea base"))
                    .on_hover_text(
                        "Guarda una copia del estado actual para ver después qué filas \
                         insertó, borró o modificó la app.",
                    )
                    .clicked()
                {
                    self.set_baseline();
                }
            }
            Some(baseline) => {
                let age = format_age(baseline.taken_at.elapsed());
                ui.horizontal(|ui| {
                    theme::dot(ui, theme::ACCENT, 3.5);
                    ui.label(
                        RichText::new(format!("Línea base · {age}"))
                            .size(12.0)
                            .color(theme::MUTED),
                    );
                });
                ui.horizontal(|ui| {
                    let label = if self.diff.is_some() {
                        "Ocultar cambios"
                    } else {
                        "Ver cambios"
                    };
                    if ui.add_enabled(!pulling, egui::Button::new(label)).clicked() {
                        if self.diff.is_some() {
                            self.diff = None;
                        } else {
                            self.compare_with_baseline();
                        }
                    }
                    if ui
                        .add_enabled(!pulling, egui::Button::new("Refijar"))
                        .on_hover_text("Toma el estado actual como nueva línea base.")
                        .clicked()
                    {
                        self.set_baseline();
                    }
                    let discard =
                        egui::Button::new(RichText::new("×").size(15.0).color(theme::SUBTLE))
                            .frame_when_inactive(false);
                    if ui
                        .add(discard)
                        .on_hover_text("Descartar la línea base")
                        .clicked()
                    {
                        self.clear_baseline();
                    }
                });
            }
        }
    }

    fn diff_card(&mut self, ui: &mut egui::Ui) {
        let Some(diff) = self.diff.take() else {
            return;
        };
        let mut close = false;
        let mut recompute = false;

        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(ui.available_height());
            ui.spacing_mut().item_spacing.y = 0.0;

            Frame::NONE
                .inner_margin(Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(theme::strong("Cambios desde la línea base"));
                        if let Some(baseline) = &self.baseline {
                            ui.label(
                                RichText::new(format_age(baseline.taken_at.elapsed()))
                                    .color(theme::MUTED),
                            );
                        }
                        let (inserted, deleted, updated) = diff.totals();
                        change_counts(ui, inserted, deleted, updated);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            close = ui.button("Cerrar").clicked();
                            recompute = ui
                                .button("Recalcular")
                                .on_hover_text("Vuelve a comparar la copia local actual.")
                                .clicked();
                        });
                    });
                });

            ui.add(egui::Separator::default().spacing(0.0));

            Frame::NONE
                .inner_margin(Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    if diff.tables.is_empty() {
                        ui.add_space(24.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                RichText::new("Sin cambios desde la línea base")
                                    .family(theme::semibold())
                                    .color(theme::TEXT_SOFT),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(if self.target.is_some() {
                                    "Usa la app y pulsa Refrescar, o activa Live Watch."
                                } else {
                                    "Modifica el archivo y pulsa Recalcular."
                                })
                                .size(12.0)
                                .color(theme::SUBTLE),
                            );
                        });
                        return;
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("diff_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 6.0;
                            for table in &diff.tables {
                                table_diff(ui, table);
                            }
                        });
                });
        });

        if !close {
            self.diff = Some(diff);
        }
        if recompute {
            self.compare_with_baseline();
        }
    }

    fn central_panel(&mut self, ui: &mut egui::Ui) {
        let frame = Frame::NONE.fill(theme::BG).inner_margin(Margin::same(16));
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            if self.db_path.is_none() {
                self.welcome(ui);
                return;
            }
            if self.diff.is_some() {
                self.diff_card(ui);
                return;
            }
            self.editor_card(ui);
            ui.add_space(12.0);
            self.results_card(ui);
        });
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        let hovering_file = ui.ctx().input(|input| !input.raw.hovered_files.is_empty());
        egui::ScrollArea::vertical()
            .id_salt("welcome_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    let width = ui.available_width().min(760.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(width, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.label(
                                theme::strong("Conecta una base de datos")
                                    .size(24.0)
                                    .color(theme::TEXT),
                            );
                            ui.add_space(2.0);
                            ui.label(
                                RichText::new(
                                    "Abre un archivo SQLite local o extrae la base de datos de tu app desde el simulador o el emulador.",
                                )
                                .size(14.0)
                                .color(theme::MUTED),
                            );
                            ui.add_space(20.0);

                            let sources = [
                                (SourceMode::Local, ".db, .sqlite o .sqlite3 desde el disco"),
                                (SourceMode::Ios, "Necesita Xcode y el simulador encendido"),
                                (SourceMode::Android, "Vía adb con run-as (APK debug)"),
                            ];
                            let mut picked = None;
                            ui.columns(sources.len(), |columns| {
                                for (column, (mode, description)) in
                                    columns.iter_mut().zip(sources)
                                {
                                    let active = self.settings.mode == mode;
                                    if source_card(column, mode, description, active) {
                                        picked = Some(mode);
                                    }
                                }
                            });
                            if let Some(mode) = picked {
                                self.choose_source(mode);
                            }
                            ui.add_space(16.0);

                            let drop_stroke = if hovering_file {
                                Stroke::new(1.5, theme::ACCENT)
                            } else {
                                Stroke::new(1.5, theme::BORDER_STRONG)
                            };
                            Frame::NONE
                                .fill(theme::SIDEBAR)
                                .stroke(drop_stroke)
                                .corner_radius(CornerRadius::same(12))
                                .inner_margin(Margin::same(28))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.vertical_centered(|ui| {
                                        ui.label(
                                            RichText::new("Arrastra un archivo .db aquí")
                                                .size(14.5)
                                                .color(theme::TEXT),
                                        );
                                        ui.weak("o");
                                        if ui
                                            .add(theme::primary_button("Abrir archivo SQLite…"))
                                            .clicked()
                                        {
                                            self.pick_local_file();
                                        }
                                    });
                                });

                            if !self.settings.recents.is_empty() {
                                ui.add_space(20.0);
                                theme::section_label(ui, "Recientes");
                                let mut reopen = None;
                                for recent in &self.settings.recents {
                                    let tag = match recent.mode {
                                        SourceMode::Local => "Local",
                                        SourceMode::Ios => "iOS",
                                        SourceMode::Android => "Android",
                                    };
                                    let button = egui::Button::new((
                                        RichText::new(tag).size(11.5).color(theme::MUTED),
                                        theme::mono(recent.label()).color(theme::TEXT),
                                    ))
                                    .right_text(
                                        RichText::new("Reconectar").size(12.0).color(theme::SUBTLE),
                                    )
                                    .min_size(egui::vec2(ui.available_width(), 40.0));
                                    if ui.add(button).clicked() {
                                        reopen = Some(recent.clone());
                                    }
                                }
                                if let Some(recent) = reopen {
                                    self.open_recent(recent);
                                }
                            }
                        },
                    );
                    ui.add_space(24.0);
                });
            });
    }

    fn choose_source(&mut self, mode: SourceMode) {
        if mode == SourceMode::Local {
            self.pick_local_file();
            return;
        }
        if self.settings.mode != mode {
            self.settings.mode = mode;
            self.detected_apps.clear();
            self.detected_dbs.clear();
            self.persist();
        }
        self.detect_devices(false);
    }

    fn editor_card(&mut self, ui: &mut egui::Ui) {
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;

            Frame::NONE
                .inner_margin(Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.horizontal(|ui| {
                        let shortcut = if ui.ctx().os() == egui::os::OperatingSystem::Mac {
                            "⌘ ↵"
                        } else {
                            "Ctrl ↵"
                        };
                        let run = theme::primary_button("▶  Ejecutar").shortcut_text(
                            RichText::new(shortcut)
                                .size(11.5)
                                .color(theme::ON_ACCENT.gamma_multiply(0.7)),
                        );
                        if ui.add(run).clicked() {
                            self.run_sql();
                        }
                        if ui
                            .add_enabled(self.db_path.is_some(), egui::Button::new("Refrescar"))
                            .clicked()
                        {
                            self.refresh();
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Limpiar").clicked() {
                                self.result = QueryResult::default();
                                self.selected = None;
                                self.generated_sql = None;
                                self.editing = None;
                                self.sql = DEFAULT_SQL.to_string();
                                self.status = "Vista limpiada.".to_string();
                            }

                            let can_export = !self.result.rows.is_empty();
                            if ui
                                .add_enabled(can_export, egui::Button::new("Exportar JSON"))
                                .clicked()
                            {
                                self.export_json();
                            }
                            if ui
                                .add_enabled(can_export, egui::Button::new("Exportar CSV"))
                                .clicked()
                            {
                                self.export_csv();
                            }
                        });
                    });
                });

            ui.add(egui::Separator::default().spacing(0.0));

            Frame::NONE
                .inner_margin(Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        let lines = self.sql.split('\n').count();
                        let digits = lines.to_string().len().max(2);
                        let numbers = (1..=lines)
                            .map(|line| format!("{line:>digits$}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        ui.add(
                            egui::Label::new(theme::mono(numbers).color(theme::FAINT))
                                .selectable(false),
                        );
                        ui.add_space(6.0);

                        // Sin ajuste de línea para que cada número quede alineado con su línea.
                        let font_id = egui::TextStyle::Monospace.resolve(ui.style());
                        let mut layouter =
                            |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, _wrap_width: f32| {
                                let job =
                                    sql_highlight::layout_job(buffer.as_str(), font_id.clone());
                                ui.fonts_mut(|fonts| fonts.layout_job(job))
                            };
                        let editor = egui::ScrollArea::horizontal()
                            .id_salt("sql_scroll")
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.sql)
                                        .frame(Frame::NONE)
                                        .margin(Margin::ZERO)
                                        .desired_rows(4)
                                        .desired_width(f32::INFINITY)
                                        .code_editor()
                                        .layouter(&mut layouter),
                                )
                            })
                            .inner;
                        if editor.has_focus()
                            && ui.input(|input| {
                                input.modifiers.command && input.key_pressed(egui::Key::Enter)
                            })
                        {
                            self.run_sql();
                        }
                    });
                });
        });
    }

    fn results_card(&mut self, ui: &mut egui::Ui) {
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(ui.available_height());
            ui.spacing_mut().item_spacing.y = 0.0;

            Frame::NONE
                .inner_margin(Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let title = self
                            .selected
                            .as_ref()
                            .map(|object| object.name.clone())
                            .unwrap_or_else(|| "Consulta".to_string());
                        ui.label(theme::mono(title).color(theme::TEXT));

                        if self.selected.is_some() {
                            let tabs = [(ResultsTab::Data, "Datos"), (ResultsTab::Schema, "Esquema")];
                            if theme::segmented(ui, &mut self.results_tab, &tabs) {
                                self.editing = None;
                            }
                        }
                        if self.results_tab == ResultsTab::Schema && self.selected.is_some() {
                            return;
                        }

                        if !self.result.columns.is_empty() {
                            let rows = self.result.rows.len();
                            let summary = match self.result.total {
                                Some(total) if rows > 0 => {
                                    let first = self.page * db::ROW_LIMIT + 1;
                                    format!("filas {first}–{} de {total}", first + rows - 1)
                                }
                                Some(total) => format!("{total} filas"),
                                None => format!("{rows} filas"),
                            };
                            ui.label(
                                RichText::new(format!(
                                    "{summary} · {} ms",
                                    self.result.elapsed.as_millis()
                                ))
                                .color(theme::MUTED),
                            );
                            if self.result.is_editable() {
                                theme::badge(
                                    ui,
                                    "Editable",
                                    theme::SUCCESS,
                                    theme::SUCCESS.gamma_multiply(0.14),
                                );
                            } else {
                                theme::badge(ui, "Solo lectura", theme::MUTED, theme::RAISED);
                            }
                        }

                        let paged = self.result.total.filter(|total| last_page(*total) > 0);
                        if paged.is_some() || self.result.is_editable() {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if let Some(total) = paged {
                                        self.pager(ui, last_page(total));
                                        ui.add_space(8.0);
                                    }
                                    if !self.result.is_editable() {
                                        return;
                                    }
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(
                                                "Doble clic para editar · Enter guarda · Esc cancela · NULL vacía",
                                            )
                                            .size(12.0)
                                            .color(theme::SUBTLE),
                                        )
                                        .truncate(),
                                    );
                                },
                            );
                        }
                    });
                });

            ui.add(egui::Separator::default().spacing(0.0));

            Frame::NONE
                .inner_margin(Margin::symmetric(14, 8))
                .show(ui, |ui| {
                    if self.results_tab == ResultsTab::Schema && self.selected.is_some() {
                        self.render_schema(ui);
                    } else {
                        self.render_results(ui);
                    }
                });
        });
    }

    fn render_schema(&mut self, ui: &mut egui::Ui) {
        let Some(name) = self.selected.as_ref().map(|object| object.name.clone()) else {
            return;
        };
        if self.tasks.is_running(TaskKind::Pull) {
            ui.weak("Extrayendo la base de datos…");
            return;
        }
        if self
            .schema
            .as_ref()
            .is_none_or(|(loaded, _)| *loaded != name)
        {
            let schema = match &self.db_path {
                Some(path) => db::open(path)
                    .and_then(|conn| db::table_schema(&conn, &name))
                    .map_err(|error| format!("{error:#}")),
                None => return,
            };
            self.schema = Some((name, schema));
        }
        let Some((_, schema)) = &self.schema else {
            return;
        };
        match schema {
            Ok(schema) => schema_view(ui, schema),
            Err(error) => {
                ui.label(
                    RichText::new(format!("No se pudo leer el esquema: {error}"))
                        .color(theme::DANGER),
                );
            }
        }
    }

    /// Controles de página, de derecha a izquierda: » › «Página n de m» ‹ «.
    fn pager(&mut self, ui: &mut egui::Ui, last_page: usize) {
        let page = self.page;
        let mut target = None;
        ui.spacing_mut().item_spacing.x = 2.0;
        let mut nav = |ui: &mut egui::Ui, text: &str, enabled: bool, to: usize, hint: &str| {
            let button = egui::Button::new(theme::mono(text)).min_size(egui::vec2(26.0, 24.0));
            if ui
                .add_enabled(enabled, button)
                .on_hover_text(hint)
                .clicked()
            {
                target = Some(to);
            }
        };
        nav(ui, "»", page < last_page, last_page, "Última página");
        nav(ui, "›", page < last_page, page + 1, "Página siguiente");
        ui.label(
            RichText::new(format!("Página {} de {}", page + 1, last_page + 1))
                .size(12.0)
                .color(theme::MUTED),
        );
        nav(ui, "‹", page > 0, page.saturating_sub(1), "Página anterior");
        nav(ui, "«", page > 0, 0, "Primera página");
        if let Some(page) = target {
            self.go_to_page(page);
        }
    }

    fn render_results(&mut self, ui: &mut egui::Ui) {
        if self.result.columns.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("Sin resultados")
                        .family(theme::semibold())
                        .color(theme::TEXT_SOFT),
                );
                ui.add_space(4.0);
                ui.weak("Ejecuta una consulta o selecciona una tabla.");
            });
            return;
        }

        let editable = self.result.is_editable();
        let mut commit: Option<(usize, usize, String)> = None;
        let mut cancel = false;

        egui::ScrollArea::both()
            .id_salt("results_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("results_grid")
                    .striped(true)
                    .min_col_width(72.0)
                    .min_row_height(26.0)
                    .spacing(egui::vec2(20.0, 4.0))
                    .show(ui, |ui| {
                        ui.label(theme::mono("#").color(theme::FAINT));
                        for column in &self.result.columns {
                            ui.label(
                                theme::strong(column.as_str())
                                    .size(12.5)
                                    .color(theme::MUTED),
                            );
                        }
                        ui.end_row();

                        for (row_index, row) in self.result.rows.iter().enumerate() {
                            ui.label(theme::mono((row_index + 1).to_string()).color(theme::FAINT));

                            for (column_index, value) in row.iter().enumerate() {
                                let is_editing = self
                                    .editing
                                    .as_ref()
                                    .map(|edit| {
                                        edit.row == row_index && edit.column == column_index
                                    })
                                    .unwrap_or(false);

                                if is_editing {
                                    let Some(mut edit) = self.editing.take() else {
                                        continue;
                                    };
                                    let response = ui.add(
                                        egui::TextEdit::singleline(&mut edit.buffer)
                                            .desired_width(180.0)
                                            .font(egui::TextStyle::Monospace)
                                            .frame(
                                                Frame::NONE
                                                    .fill(theme::BG)
                                                    .stroke(Stroke::new(1.0, theme::ACCENT))
                                                    .corner_radius(CornerRadius::same(4))
                                                    .inner_margin(Margin::symmetric(6, 3)),
                                            ),
                                    );
                                    if edit.focus_pending {
                                        response.request_focus();
                                        edit.focus_pending = false;
                                    }
                                    if response.lost_focus() {
                                        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
                                            cancel = true;
                                            self.editing = None;
                                        } else {
                                            commit = Some((
                                                row_index,
                                                column_index,
                                                edit.buffer.clone(),
                                            ));
                                            self.editing = Some(edit);
                                        }
                                    } else {
                                        self.editing = Some(edit);
                                    }
                                } else {
                                    let sense = if editable {
                                        egui::Sense::click()
                                    } else {
                                        egui::Sense::hover()
                                    };
                                    let response =
                                        ui.add(egui::Label::new(cell_text(value)).sense(sense));
                                    if response.double_clicked() {
                                        self.editing = Some(CellEdit {
                                            row: row_index,
                                            column: column_index,
                                            buffer: if matches!(value, Value::Null) {
                                                String::new()
                                            } else {
                                                db::value_display(value)
                                            },
                                            focus_pending: true,
                                        });
                                    }
                                }
                            }
                            ui.end_row();
                        }
                    });
            });

        if cancel {
            self.editing = None;
            self.status = "Edición cancelada.".to_string();
        }
        if let Some((row, column, value)) = commit {
            self.editing = None;
            self.apply_edit(row, column, value);
        }
    }
}

/// Tarjeta clicable de origen para la pantalla de bienvenida.
fn source_card(ui: &mut egui::Ui, mode: SourceMode, description: &str, active: bool) -> bool {
    let stroke = if active {
        Stroke::new(1.0, theme::ACCENT)
    } else {
        Stroke::new(1.0, theme::BORDER)
    };
    let frame = theme::card()
        .stroke(stroke)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(64.0);
            ui.label(theme::strong(mode.label()).size(14.0).color(theme::TEXT));
            ui.add_space(2.0);
            ui.label(RichText::new(description).size(12.5).color(theme::MUTED));
        });
    let rect = frame.response.rect;
    let response = ui
        .interact(rect, ui.scope_id().with(mode.tag()), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.hovered() && !active {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(12),
            Stroke::new(1.0, Color32::from_rgb(0x46, 0x4D, 0x5B)),
            egui::StrokeKind::Inside,
        );
    }
    response.clicked()
}

fn schema_view(ui: &mut egui::Ui, schema: &db::TableSchema) {
    egui::ScrollArea::vertical()
        .id_salt("schema_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;

            schema_section(ui, "Columnas", schema.columns.len());
            schema_grid(
                ui,
                "schema_columns",
                &["", "Nombre", "Tipo", "Nulos", "Por defecto"],
                schema.columns.iter().map(|column| {
                    vec![
                        match column.primary_key {
                            0 => RichText::new(""),
                            position => RichText::new(format!("PK{position}"))
                                .size(11.0)
                                .color(theme::WARNING),
                        },
                        theme::mono(column.name.as_str()).color(theme::TEXT),
                        theme::mono(column.decl_type.as_str()).color(theme::KEYWORD),
                        if column.not_null {
                            RichText::new("NOT NULL").size(12.0).color(theme::TEXT_SOFT)
                        } else {
                            RichText::new("sí").size(12.0).color(theme::SUBTLE)
                        },
                        match &column.default {
                            Some(default) => theme::mono(default.as_str()).color(theme::STRING),
                            None => RichText::new("—").color(theme::FAINT),
                        },
                    ]
                }),
            );

            if !schema.indexes.is_empty() {
                schema_section(ui, "Índices", schema.indexes.len());
                schema_grid(
                    ui,
                    "schema_indexes",
                    &["Nombre", "Columnas", "Tipo"],
                    schema.indexes.iter().map(|index| {
                        let mut kind = match index.origin.as_str() {
                            "pk" => "clave primaria",
                            "u" => "restricción UNIQUE",
                            _ if index.unique => "UNIQUE",
                            _ => "índice",
                        }
                        .to_string();
                        if index.partial {
                            kind.push_str(" · parcial");
                        }
                        vec![
                            theme::mono(index.name.as_str()).color(theme::TEXT),
                            theme::mono(index.columns.join(", ")).color(theme::IDENT),
                            RichText::new(kind).size(12.0).color(theme::MUTED),
                        ]
                    }),
                );
            }

            if !schema.foreign_keys.is_empty() {
                schema_section(ui, "Claves foráneas", schema.foreign_keys.len());
                schema_grid(
                    ui,
                    "schema_foreign_keys",
                    &["Columnas", "Referencia", "ON UPDATE", "ON DELETE"],
                    schema.foreign_keys.iter().map(|key| {
                        vec![
                            theme::mono(key.from.join(", ")).color(theme::TEXT),
                            theme::mono(format!("{}({})", key.table, key.to.join(", ")))
                                .color(theme::IDENT),
                            RichText::new(key.on_update.as_str())
                                .size(12.0)
                                .color(theme::MUTED),
                            RichText::new(key.on_delete.as_str())
                                .size(12.0)
                                .color(theme::MUTED),
                        ]
                    }),
                );
            }

            // Sentencias originales: la tabla o vista, los índices creados a mano y los triggers.
            let statements: Vec<&str> = schema
                .sql
                .iter()
                .map(String::as_str)
                .chain(
                    schema
                        .indexes
                        .iter()
                        .filter_map(|index| index.sql.as_deref()),
                )
                .chain(schema.triggers.iter().map(|(_, sql)| sql.as_str()))
                .collect();
            if !statements.is_empty() {
                let triggers = if schema.triggers.is_empty() {
                    String::new()
                } else {
                    format!(" · {} triggers", schema.triggers.len())
                };
                schema_section_text(ui, &format!("SQL{triggers}"));
                let font_id = egui::TextStyle::Monospace.resolve(ui.style());
                for sql in statements {
                    theme::card().inner_margin(Margin::same(10)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let job = sql_highlight::layout_job(&format!("{sql};"), font_id.clone());
                        ui.add(egui::Label::new(job).selectable(true));
                    });
                }
            }
        });
}

fn schema_section(ui: &mut egui::Ui, title: &str, count: usize) {
    schema_section_text(ui, &format!("{title} · {count}"));
}

fn schema_section_text(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).color(theme::TEXT_SOFT));
}

fn schema_grid(
    ui: &mut egui::Ui,
    id: &str,
    headers: &[&str],
    rows: impl Iterator<Item = Vec<RichText>>,
) {
    egui::Grid::new(id)
        .striped(true)
        .spacing(egui::vec2(18.0, 4.0))
        .show(ui, |ui| {
            for header in headers {
                ui.label(
                    RichText::new(*header)
                        .family(theme::semibold())
                        .size(12.0)
                        .color(theme::MUTED),
                );
            }
            ui.end_row();
            for row in rows {
                for cell in row {
                    ui.label(cell);
                }
                ui.end_row();
            }
        });
}

/// `+n −n ~n` con los colores de cada tipo de cambio; omite los que son cero.
fn change_counts(ui: &mut egui::Ui, inserted: u64, deleted: u64, updated: u64) {
    for (count, sign, color) in [
        (inserted, "+", theme::SUCCESS),
        (deleted, "−", theme::DANGER),
        (updated, "~", theme::WARNING),
    ] {
        if count > 0 {
            ui.label(theme::mono(format!("{sign}{count}")).color(color));
        }
    }
}

/// Una tabla de la comparación: cabecera plegable con sus contadores y, dentro, las filas
/// insertadas, eliminadas y modificadas.
fn table_diff(ui: &mut egui::Ui, table: &TableDiff) {
    let id = ui.make_persistent_id(("diff_table", table.name.as_str()));
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true)
        .show_header(ui, |ui| {
            ui.label(theme::mono(table.name.as_str()).color(theme::TEXT));
            match table.change {
                TableChange::Added => {
                    let bg = theme::SUCCESS.gamma_multiply(0.14);
                    theme::badge(ui, "nueva", theme::SUCCESS, bg);
                }
                TableChange::Removed => {
                    let bg = theme::DANGER.gamma_multiply(0.14);
                    theme::badge(ui, "eliminada", theme::DANGER, bg);
                }
                TableChange::Modified => {}
            }
            if table.schema_changed {
                theme::badge(ui, "esquema cambiado", theme::WARNING, theme::RAISED)
                    .on_hover_text("Solo se comparan las columnas que existen en ambas versiones.");
            }
            change_counts(
                ui,
                table.inserted.count,
                table.deleted.count,
                table.updated.count,
            );
        })
        .body(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            if table.inserted.count > 0 {
                diff_section(ui, "Insertadas", theme::SUCCESS, table.inserted.count);
                let rows = table.inserted.rows.iter().map(|row| (row.as_slice(), None));
                diff_grid(ui, (&table.name, "ins"), &table.columns, rows);
            }
            if table.deleted.count > 0 {
                diff_section(ui, "Eliminadas", theme::DANGER, table.deleted.count);
                let rows = table.deleted.rows.iter().map(|row| (row.as_slice(), None));
                diff_grid(ui, (&table.name, "del"), &table.columns, rows);
            }
            if table.updated.count > 0 {
                diff_section(ui, "Modificadas", theme::WARNING, table.updated.count);
                let rows = table
                    .updated
                    .rows
                    .iter()
                    .map(|row| (row.after.as_slice(), Some(row.before.as_slice())));
                diff_grid(ui, (&table.name, "upd"), &table.columns, rows);
            }
        });
}

fn diff_section(ui: &mut egui::Ui, title: &str, color: Color32, count: u64) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        theme::dot(ui, color, 3.5);
        ui.label(RichText::new(format!("{title} · {count}")).color(theme::TEXT_SOFT));
        if count > diff::MAX_ROWS as u64 {
            ui.label(
                RichText::new(format!("se muestran las primeras {}", diff::MAX_ROWS))
                    .size(12.0)
                    .color(theme::SUBTLE),
            );
        }
    });
}

/// Rejilla de filas. Si una fila trae su versión anterior, las celdas que cambiaron se pintan
/// como `antes → después` y el resto se atenúa.
fn diff_grid<'a>(
    ui: &mut egui::Ui,
    id: (&str, &str),
    columns: &[String],
    rows: impl Iterator<Item = (&'a [Value], Option<&'a [Value]>)>,
) {
    egui::ScrollArea::horizontal()
        .id_salt(("diff_scroll", id))
        .show(ui, |ui| {
            egui::Grid::new(("diff_grid", id))
                .striped(true)
                .spacing(egui::vec2(16.0, 4.0))
                .show(ui, |ui| {
                    for column in columns {
                        ui.label(
                            RichText::new(column.as_str())
                                .family(theme::semibold())
                                .size(12.0)
                                .color(theme::MUTED),
                        );
                    }
                    ui.end_row();
                    for (after, before) in rows {
                        for (index, value) in after.iter().enumerate() {
                            match before.map(|before| &before[index]) {
                                Some(old) if old != value => {
                                    ui.label(
                                        theme::mono(format!(
                                            "{} → {}",
                                            db::value_display(old),
                                            db::value_display(value)
                                        ))
                                        .color(theme::WARNING),
                                    );
                                }
                                Some(_) => {
                                    ui.label(
                                        theme::mono(db::value_display(value)).color(theme::FAINT),
                                    );
                                }
                                None => {
                                    ui.label(cell_text(value));
                                }
                            }
                        }
                        ui.end_row();
                    }
                });
        });
}

fn format_age(elapsed: Duration) -> String {
    match elapsed.as_secs() {
        0..=59 => "hace menos de 1 min".to_string(),
        seconds @ 60..=3599 => format!("hace {} min", seconds / 60),
        seconds => format!("hace {} h {} min", seconds / 3600, seconds % 3600 / 60),
    }
}

/// Índice de la última página de una tabla con `total` filas (0 si está vacía).
fn last_page(total: u64) -> usize {
    (total as usize).saturating_sub(1) / db::ROW_LIMIT
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn cell_text(value: &Value) -> RichText {
    match value {
        Value::Null => RichText::new(" NULL ")
            .monospace()
            .italics()
            .color(theme::SUBTLE)
            .background_color(theme::HOVER),
        Value::Integer(_) | Value::Real(_) => {
            theme::mono(db::value_display(value)).color(theme::NUMBER)
        }
        Value::Blob(_) => theme::mono(db::value_display(value)).color(theme::MUTED),
        Value::Text(_) => theme::mono(db::value_display(value)).color(theme::TEXT),
    }
}

impl eframe::App for DbBridgeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_tasks();
        self.handle_dropped_files(&ctx);
        self.tick_live_watch(&ctx);

        self.top_panel(ui);
        self.status_panel(ui);
        self.side_panel(ui);
        self.central_panel(ui);
    }
}
