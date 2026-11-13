use crate::db::{self, DbObject, ObjectKind, QueryResult};
use crate::diff::{self, DatabaseDiff, TableChange, TableDiff};
use crate::favorites;
use crate::mobile::{self, Device};
use crate::model::{FavoriteQuery, RecentSource, Settings, SourceMode, ThemeMode};
use crate::sql_highlight;
use crate::tasks::{TaskKind, Tasks};
use crate::theme;
use crate::update_check::{self, Release};
use crate::updater::{self, Installation};
use crate::value_view;
use eframe::egui::{self, Color32, CornerRadius, Frame, Margin, RichText, Stroke};
use rusqlite::types::Value;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const DEFAULT_SQL: &str =
    "SELECT name, type FROM sqlite_master WHERE type IN ('table', 'view') ORDER BY name;";
/// Intervalos de Live Watch en segundos (0 = apagado) y su etiqueta.
const LIVE_WATCH_OPTIONS: [(u64, &str); 5] = [
    (0, "Apagado"),
    (2, "2 s"),
    (5, "5 s"),
    (30, "30 s"),
    (60, "1 min"),
];
const MAX_RECENTS: usize = 5;
const MAX_HISTORY: usize = 30;
/// Altura del pie bajo la rejilla (Live Watch y paginación): separador, márgenes y controles.
const FOOTER_HEIGHT: f32 = 48.0;

struct CellEdit {
    row: usize,
    column: usize,
    buffer: String,
    focus_pending: bool,
}

/// Acción pedida desde el menú contextual de una celda; se aplica después de pintar la rejilla.
enum CellAction {
    View(usize, usize),
    Copy(usize, usize),
    SetNull(usize, usize),
    Delete(usize),
}

/// Valor abierto en el visor, con su presentación ya calculada.
struct ValueViewer {
    title: String,
    /// Tipo, tamaño y formato detectado.
    summary: String,
    body: String,
    blob: Option<Vec<u8>>,
}

#[derive(Clone, Copy, PartialEq)]
enum FieldMode {
    Default,
    Null,
    Value,
}

struct InsertField {
    column: db::ColumnInfo,
    mode: FieldMode,
    text: String,
}

struct InsertForm {
    table: String,
    fields: Vec<InsertField>,
    error: Option<String>,
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

/// Detección que terminó sin resultado o con error. Se muestra junto a «Detectar» hasta la
/// siguiente, porque en la barra de estado pasa desapercibida.
struct DetectNotice {
    message: String,
    /// Modo al que ofrecer el cambio porque tiene dispositivos encendidos.
    switch_to: Option<SourceMode>,
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
        /// El otro modo móvil, si no se encontró nada en este y allí sí hay algo encendido.
        elsewhere: Option<SourceMode>,
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
    LatestRelease(anyhow::Result<Release>),
    UpdateInstalled {
        version: String,
        result: anyhow::Result<()>,
    },
}

/// Resultado de instalar una versión nueva desde la banda de aviso.
enum UpdateInstall {
    /// Versión ya instalada en disco; se usa al reiniciar.
    Installed(String),
    Failed(String),
}

/// Una conexión con su propio origen, consulta, resultados y tareas. Cada pestaña es
/// independiente: se puede tener un simulador de iOS en una y un emulador Android en otra, o dos
/// archivos de la misma app.
struct Tab {
    /// Identifica la pestaña para que no compartan archivos temporales (línea base).
    id: u64,
    mode: SourceMode,
    /// UDID o serial del dispositivo elegido; vacío para usar el único encendido/conectado.
    device: String,
    app_id: String,
    db_name: String,
    detected_devices: Vec<Device>,
    detected_apps: Vec<String>,
    detected_dbs: Vec<String>,
    /// El usuario cambió de dispositivo o de app: al detectar sus bases de datos se extrae y
    /// conecta la elegida sin esperar a «Extraer y conectar».
    connect_after_detect: bool,
    db_path: Option<PathBuf>,
    /// Origen de la copia local cuando se extrajo de un dispositivo.
    target: Option<MobileTarget>,
    origin: Option<String>,
    objects: Vec<DbObject>,
    selected: Option<DbObject>,
    table_filter: String,
    sql: String,
    generated_sql: Option<String>,
    /// Página, orden y filtro de la tabla seleccionada mientras se explora.
    view: db::TableView,
    result: QueryResult,
    results_tab: ResultsTab,
    /// Esquema de la tabla seleccionada, cargado al abrir la pestaña y guardado con su nombre.
    schema: Option<(String, Result<db::TableSchema, String>)>,
    viewer: Option<ValueViewer>,
    insert_form: Option<InsertForm>,
    /// Fila (índice en `result.rows`) pendiente de confirmar su borrado.
    pending_delete: Option<usize>,
    editing: Option<CellEdit>,
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
    detect_notice: Option<DetectNotice>,
}

impl Tab {
    fn new(ctx: &egui::Context, id: u64, mode: SourceMode) -> Self {
        Self {
            id,
            mode,
            device: String::new(),
            app_id: String::new(),
            db_name: String::new(),
            detected_devices: Vec::new(),
            detected_apps: Vec::new(),
            detected_dbs: Vec::new(),
            connect_after_detect: false,
            db_path: None,
            target: None,
            origin: None,
            objects: Vec::new(),
            selected: None,
            table_filter: String::new(),
            sql: DEFAULT_SQL.to_string(),
            generated_sql: None,
            view: db::TableView::default(),
            result: QueryResult::default(),
            results_tab: ResultsTab::Data,
            schema: None,
            viewer: None,
            insert_form: None,
            pending_delete: None,
            editing: None,
            last_refresh: Instant::now(),
            dirty: false,
            change_revision: 0,
            queued_push: None,
            tasks: Tasks::new(ctx.clone()),
            baseline: None,
            diff: None,
            status: "Abre un archivo o extrae la base de datos de un emulador.".to_string(),
            detect_notice: None,
        }
    }

    /// Pestaña nueva sin conexión con el mismo dispositivo, app y archivo elegidos que esta, para
    /// abrir otro archivo de la misma app sin volver a detectarlo todo.
    fn sibling(&self, ctx: &egui::Context, id: u64) -> Self {
        let mut tab = Self::new(ctx, id, self.mode);
        tab.device = self.device.clone();
        tab.app_id = self.app_id.clone();
        tab.db_name = self.db_name.clone();
        tab.detected_devices = self.detected_devices.clone();
        tab.detected_apps = self.detected_apps.clone();
        tab.detected_dbs = self.detected_dbs.clone();
        tab.status = match self.mode {
            SourceMode::Local => "Abre un archivo SQLite o arrástralo a la ventana.".to_string(),
            _ => "Elige otro archivo: se extraerá y conectará al momento.".to_string(),
        };
        tab
    }

    /// Texto de la pestaña: plataforma y archivo conectado, o el modo si no hay conexión.
    fn title(&self) -> String {
        let platform = match self.mode {
            SourceMode::Local => "Local",
            SourceMode::Ios => "iOS",
            SourceMode::Android => "Android",
        };
        match (&self.target, &self.db_path) {
            (Some(target), Some(_)) => format!("{platform} · {}", last_segment(&target.db_name)),
            (None, Some(path)) => format!("{platform} · {}", file_name(path)),
            _ => format!("{platform} · sin conexión"),
        }
    }

    /// Cerrarla perdería cambios sin enviar o cortaría una transferencia.
    fn has_pending_work(&self) -> bool {
        self.dirty || self.tasks.is_running(TaskKind::Pull) || self.tasks.is_running(TaskKind::Push)
    }
}

pub struct DbBridgeApp {
    settings: Settings,
    /// Pestaña activa. Su hueco en `tabs` guarda una vacía hasta que se cambia a otra.
    tab: Tab,
    tabs: Vec<Tab>,
    active: usize,
    next_tab_id: u64,
    /// Pestaña con trabajo pendiente cuyo cierre espera confirmación.
    pending_close: Option<usize>,
    ctx: egui::Context,
    /// Búsqueda e instalación de actualizaciones: no pertenecen a ninguna pestaña.
    tasks: Tasks<TaskOutput>,
    /// Instalación en marcha, o por qué no se puede actualizar desde la app.
    installation: Result<Installation, String>,
    update_install: Option<UpdateInstall>,
    new_favorite_name: String,
}

impl DbBridgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let settings = favorites::load();
        theme::install(&cc.egui_ctx, settings.theme);

        // La primera pestaña recupera la última selección guardada.
        let mut tab = Tab::new(&cc.egui_ctx, 0, settings.mode);
        tab.device = settings.device.clone();
        tab.app_id = settings.app_id.clone();
        tab.db_name = settings.db_name.clone();
        let mut app = Self {
            settings,
            tabs: vec![Tab::new(&cc.egui_ctx, 0, SourceMode::Local)],
            tab,
            active: 0,
            next_tab_id: 1,
            pending_close: None,
            ctx: cc.egui_ctx.clone(),
            tasks: Tasks::new(cc.egui_ctx.clone()),
            installation: Installation::current().map_err(|error| format!("{error:#}")),
            update_install: None,
            new_favorite_name: String::new(),
        };
        app.check_for_updates(false);
        app
    }

    /// Abre una pestaña junto a la activa con su misma selección y pasa a ella.
    fn new_tab(&mut self) {
        let tab = self.tab.sibling(&self.ctx, self.next_tab_id);
        self.next_tab_id += 1;
        let index = self.active + 1;
        self.tabs.insert(index, tab);
        self.switch_tab(index);
        if self.tab.mode != SourceMode::Local && self.tab.detected_devices.is_empty() {
            self.detect_devices(false);
        }
    }

    fn switch_tab(&mut self, index: usize) {
        if index == self.active || index >= self.tabs.len() {
            return;
        }
        std::mem::swap(&mut self.tab, &mut self.tabs[self.active]);
        std::mem::swap(&mut self.tab, &mut self.tabs[index]);
        self.active = index;
        // Con Live Watch activo, la pestaña a la que se vuelve se refresca al momento.
        let interval = Duration::from_secs(self.settings.live_watch_secs);
        if let Some(due) = Instant::now().checked_sub(interval) {
            self.tab.last_refresh = due;
        }
        self.persist();
    }

    /// Cierra la pestaña o pide confirmación si tiene cambios sin enviar o una transferencia.
    fn request_close_tab(&mut self, index: usize) {
        if self.tab_at(index).has_pending_work() {
            self.pending_close = Some(index);
        } else {
            self.close_tab(index);
        }
    }

    fn close_tab(&mut self, index: usize) {
        if self.tabs.len() == 1 {
            // La última no se quita: se sustituye por una vacía del mismo modo.
            self.tab = Tab::new(&self.ctx, self.next_tab_id, self.tab.mode);
            self.next_tab_id += 1;
            return;
        }
        if index == self.active {
            let next = if index + 1 < self.tabs.len() {
                index + 1
            } else {
                index - 1
            };
            self.switch_tab(next);
        }
        self.tabs.remove(index);
        if index < self.active {
            self.active -= 1;
        }
    }

    /// Ejecuta `job` con la pestaña `index` como activa, para terminar en ella las tareas que
    /// lanzó aunque se esté viendo otra.
    fn on_tab(&mut self, index: usize, job: impl FnOnce(&mut Self)) {
        if index == self.active {
            job(self);
            return;
        }
        std::mem::swap(&mut self.tab, &mut self.tabs[index]);
        job(self);
        std::mem::swap(&mut self.tab, &mut self.tabs[index]);
    }

    fn tab_at(&self, index: usize) -> &Tab {
        if index == self.active {
            &self.tab
        } else {
            &self.tabs[index]
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let frame = Frame::NONE
            .fill(theme::sidebar())
            .inner_margin(Margin::symmetric(12, 4));
        let mut switch_to = None;
        let mut close = None;
        let mut open = false;
        egui::Panel::top("tab_bar").frame(frame).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for index in 0..self.tabs.len() {
                    let tab = self.tab_at(index);
                    let active = index == self.active;
                    let busy = tab.tasks.running().next().is_some();
                    let color = if active {
                        theme::text()
                    } else {
                        theme::text_soft()
                    };
                    let mut hover = tab
                        .origin
                        .clone()
                        .unwrap_or_else(|| tab.mode.label().into());
                    if let Some(target) = &tab.target {
                        let device = tab
                            .detected_devices
                            .iter()
                            .find(|device| device.id == target.device)
                            .map_or(target.device.as_str(), |device| device.name.as_str());
                        if !device.is_empty() {
                            hover.push_str(&format!("\n{device}"));
                        }
                    }
                    if tab.dirty {
                        hover.push_str("\nCambios sin push");
                    }
                    ui.horizontal(|ui| {
                        if busy {
                            ui.add(egui::Spinner::new().size(10.0).color(theme::accent()));
                        } else if tab.dirty {
                            theme::dot(ui, theme::warning(), 3.0);
                        }
                        let button = egui::Button::selectable(
                            active,
                            RichText::new(tab.title()).size(12.5).color(color),
                        )
                        .min_size(egui::vec2(0.0, 26.0));
                        if ui.add(button).on_hover_text(hover).clicked() {
                            switch_to = Some(index);
                        }
                        let cross =
                            egui::Button::new(RichText::new("×").size(14.0).color(theme::subtle()))
                                .frame_when_inactive(false)
                                .min_size(egui::vec2(22.0, 22.0));
                        if ui.add(cross).on_hover_text("Cerrar pestaña").clicked() {
                            close = Some(index);
                        }
                    });
                    ui.add_space(6.0);
                }
                let add = egui::Button::new(RichText::new("+").size(15.0).color(theme::muted()))
                    .frame_when_inactive(false)
                    .min_size(egui::vec2(26.0, 26.0));
                open = ui
                    .add(add)
                    .on_hover_text(
                        "Nueva pestaña con el mismo dispositivo y app: elige otro archivo, \
                         dispositivo o modo sin perder esta conexión.",
                    )
                    .clicked();
            });
        });
        if let Some(index) = switch_to {
            self.switch_tab(index);
        }
        if let Some(index) = close {
            self.request_close_tab(index);
        }
        if open {
            self.new_tab();
        }
    }

    fn persist(&mut self) {
        // Al volver a abrir la app, la primera pestaña recupera la última selección.
        self.settings.mode = self.tab.mode;
        self.settings.device = self.tab.device.clone();
        self.settings.app_id = self.tab.app_id.clone();
        self.settings.db_name = self.tab.db_name.clone();
        if let Err(error) = favorites::save(&self.settings) {
            self.tab.status = format!("No se pudieron guardar los ajustes: {error:#}");
        }
    }

    /// Lista los dispositivos y, si hay alguno, sigue con sus apps y sus bases de datos.
    fn detect_devices(&mut self, fallback: bool) {
        let mode = self.tab.mode;
        if mode == SourceMode::Local {
            return;
        }
        self.tab.detect_notice = None;
        self.tab.connect_after_detect = false;
        self.tab.tasks.spawn(TaskKind::DetectDevices, move || {
            let result = mobile::detect_devices(mode);
            let found = result.as_ref().is_ok_and(|devices| !devices.is_empty());
            let elsewhere = other_mobile_mode(mode).filter(|other| {
                !found && mobile::detect_devices(*other).is_ok_and(|devices| !devices.is_empty())
            });
            TaskOutput::Devices {
                mode,
                fallback,
                result,
                elsewhere,
            }
        });
    }

    /// Deja la detección sin resultado a la vista: en la barra de estado y junto a «Detectar».
    fn detect_failed(&mut self, message: String, switch_to: Option<SourceMode>) {
        self.tab.status = message.clone();
        self.tab.detect_notice = Some(DetectNotice { message, switch_to });
    }

    fn change_mode(&mut self, mode: SourceMode) {
        self.tab.mode = mode;
        self.tab.detected_devices.clear();
        self.tab.detected_apps.clear();
        self.tab.detected_dbs.clear();
        self.tab.detect_notice = None;
        self.tab.editing = None;
        if mode == SourceMode::Local {
            self.tab.status = "Abre un archivo SQLite o arrástralo a la ventana.".to_string();
        } else {
            self.tab.status = format!("{}: buscando dispositivos…", mode.label());
            self.detect_devices(false);
        }
        self.persist();
    }

    fn finish_detect_devices(
        &mut self,
        mode: SourceMode,
        fallback: bool,
        result: anyhow::Result<Vec<Device>>,
        elsewhere: Option<SourceMode>,
    ) {
        if mode != self.tab.mode {
            return;
        }
        let hint = match elsewhere {
            Some(SourceMode::Ios) => " Sí hay un simulador de iOS encendido.",
            Some(SourceMode::Android) => " Sí hay un dispositivo Android conectado.",
            _ => "",
        };
        match result {
            Ok(devices) if devices.is_empty() => {
                self.tab.detected_devices.clear();
                self.tab.detected_apps.clear();
                self.tab.detected_dbs.clear();
                let message = match mode {
                    SourceMode::Ios => "No hay ningún simulador de iOS encendido.",
                    _ => "No hay ningún dispositivo o emulador conectado a adb.",
                };
                self.detect_failed(format!("{message}{hint}"), elsewhere);
            }
            Ok(devices) => {
                // Se conserva el elegido si sigue conectado; si no, el primero de la lista.
                if !devices.iter().any(|device| device.id == self.tab.device) {
                    self.tab.device = devices[0].id.clone();
                    self.tab.detected_apps.clear();
                    self.tab.detected_dbs.clear();
                    self.persist();
                }
                self.tab.detected_devices = devices;
                self.detect_apps(fallback);
            }
            Err(error) => {
                self.tab.detected_devices.clear();
                self.detect_failed(
                    format!("No se pudieron listar los dispositivos: {error:#}.{hint}"),
                    elsewhere,
                );
            }
        }
    }

    fn select_device(&mut self, device: String) {
        if device == self.tab.device {
            return;
        }
        self.tab.device = device;
        self.tab.detected_apps.clear();
        self.tab.detected_dbs.clear();
        self.tab.connect_after_detect = true;
        self.persist();
        self.detect_apps(true);
    }

    fn detect_apps(&mut self, fallback: bool) {
        let mode = self.tab.mode;
        if mode == SourceMode::Local {
            return;
        }
        let device = self.tab.device.clone();
        self.tab.tasks.spawn(TaskKind::DetectApps, move || {
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
        if mode != self.tab.mode || device != self.tab.device {
            return;
        }
        let mut found_apps = false;
        match result {
            Ok(apps) => {
                if apps.is_empty() {
                    self.detect_failed(
                        "No se encontraron apps de usuario instaladas en el dispositivo."
                            .to_string(),
                        None,
                    );
                } else {
                    if self.tab.app_id.trim().is_empty()
                        || !apps.iter().any(|app| app == self.tab.app_id.trim())
                    {
                        self.tab.app_id = apps[0].clone();
                    }
                    self.tab.status = format!("{} aplicaciones detectadas.", apps.len());
                }
                found_apps = !apps.is_empty();
                self.tab.detected_apps = apps;
            }
            Err(error) => {
                self.detect_failed(format!("No se pudieron detectar apps: {error:#}"), None);
            }
        }
        if found_apps || (fallback && self.tab.detected_apps.is_empty()) {
            self.detect_databases();
        } else {
            self.tab.connect_after_detect = false;
        }
    }

    fn detect_databases(&mut self) {
        let mode = self.tab.mode;
        let device = self.tab.device.clone();
        let app_id = self.tab.app_id.trim().to_string();
        if mode == SourceMode::Local || app_id.is_empty() {
            self.tab.detected_dbs.clear();
            self.tab.connect_after_detect = false;
            return;
        }
        self.tab.tasks.spawn(TaskKind::DetectDatabases, move || {
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
        if mode != self.tab.mode || device != self.tab.device || app_id != self.tab.app_id.trim() {
            return;
        }
        match result {
            Ok(databases) => {
                if databases.is_empty() {
                    self.detect_failed(
                        format!(
                            "No se encontraron bases de datos en {app_id}. Escribe el nombre del archivo."
                        ),
                        None,
                    );
                } else {
                    // Un nombre guardado de antes («offline.db») se traduce a su ruta detectada.
                    let current = self.tab.db_name.trim();
                    let matching = databases
                        .iter()
                        .find(|path| {
                            path.as_str() == current || path.ends_with(&format!("/{current}"))
                        })
                        .cloned();
                    self.tab.db_name = matching.unwrap_or_else(|| databases[0].clone());
                    self.tab.status = format!(
                        "{} bases de datos encontradas en {app_id}.",
                        databases.len()
                    );
                }
                self.tab.detected_dbs = databases;
            }
            Err(error) => {
                self.tab.detected_dbs.clear();
                self.detect_failed(
                    format!("No se pudieron detectar bases de datos: {error:#}"),
                    None,
                );
            }
        }
        if std::mem::take(&mut self.tab.connect_after_detect) && !self.tab.detected_dbs.is_empty() {
            self.connect_selection();
        }
    }

    /// Se eligió otro archivo, app o dispositivo en los selectores: se extrae y conecta al
    /// momento. Con cambios sin enviar no, porque la reconexión los dejaría sin push.
    fn connect_selection(&mut self) {
        if self.tab.dirty {
            self.tab.status = "Hay cambios sin push: envíalos o pulsa «Extraer y conectar» para \
                               descartarlos y abrir el archivo elegido."
                .to_string();
            return;
        }
        self.pull_selected();
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
        self.tab.mode = SourceMode::Local;
        self.tab.detected_devices.clear();
        self.tab.detected_apps.clear();
        self.tab.detected_dbs.clear();
        let origin = file_name(&path);
        if self.connect(path.clone(), origin) {
            self.tab.target = None;
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
                    self.tab.status = format!("No se encontró {}", path.display());
                }
            }
            None => {
                self.tab.mode = recent.mode;
                // Los recientes guardados antes del selector no tienen dispositivo.
                if !recent.device.is_empty() {
                    self.tab.device = recent.device;
                }
                self.tab.app_id = recent.app_id;
                self.tab.db_name = recent.db_name;
                self.tab.detected_devices.clear();
                self.tab.detected_apps.clear();
                self.tab.detected_dbs.clear();
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
        self.tab.tasks.is_running(TaskKind::Pull) || self.tab.tasks.is_running(TaskKind::Push)
    }

    /// Extrae la base de datos elegida en los selectores.
    fn pull_selected(&mut self) {
        let mode = self.tab.mode;
        if mode == SourceMode::Local {
            return;
        }
        let app_id = self.tab.app_id.trim().to_string();
        let db_name = self.tab.db_name.trim().to_string();
        if app_id.is_empty() || db_name.is_empty() {
            self.tab.status =
                "Indica el identificador de la app y el nombre del archivo de base de datos."
                    .to_string();
            return;
        }
        self.pull_mobile(MobileTarget {
            mode,
            device: self.tab.device.clone(),
            app_id,
            db_name,
        });
    }

    fn pull_mobile(&mut self, target: MobileTarget) {
        if self.transfer_running() {
            self.tab.status = "Espera a que termine la transferencia en curso.".to_string();
            return;
        }
        self.tab.tasks.spawn(TaskKind::Pull, move || {
            let result = mobile::pull(target.mode, &target.device, &target.app_id, &target.db_name);
            TaskOutput::Pulled { target, result }
        });
    }

    fn finish_pull(&mut self, target: MobileTarget, result: anyhow::Result<PathBuf>) {
        // Se pasó a archivo local u otro modo mientras se extraía.
        if target.mode != self.tab.mode {
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
                    self.tab.target = Some(target);
                }
                self.persist();
            }
            Err(error) => {
                self.tab.status = format!("Error extrayendo la base de datos: {error:#}");
            }
        }
    }

    /// Envía la copia local al dispositivo en segundo plano. `message` describe el cambio que
    /// lo motivó, si lo hay, para el texto de la barra de estado.
    fn push_mobile(&mut self, message: Option<String>) {
        let (Some(local), Some(target)) = (self.tab.db_path.clone(), self.tab.target.clone())
        else {
            return;
        };
        if self.tab.tasks.is_running(TaskKind::Pull) {
            self.tab.status = "Espera a que termine la extracción en curso.".to_string();
            return;
        }
        if self.tab.tasks.is_running(TaskKind::Push) {
            // El push siguiente lleva todos los cambios acumulados: basta con recordar el último.
            let message = message.unwrap_or_default();
            self.tab.status = format!("{message} En cola para enviar al dispositivo.")
                .trim_start()
                .to_string();
            self.tab.queued_push = Some(message);
            return;
        }

        if let Err(error) = db::checkpoint(&local) {
            self.tab.status = format!("Error preparando la base de datos para el push: {error:#}");
            return;
        }
        let snapshot = match mobile::push_snapshot(&local) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.tab.status =
                    format!("Error preparando la base de datos para el push: {error:#}");
                return;
            }
        };

        let revision = self.tab.change_revision;
        self.tab.status = match &message {
            Some(message) => format!("{message} Enviando al dispositivo…"),
            None => "Enviando al dispositivo…".to_string(),
        };
        self.tab
            .tasks
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
                if revision == self.tab.change_revision {
                    self.tab.dirty = false;
                }
                self.tab.status = match message {
                    Some(message) => {
                        format!("{message} Enviado al dispositivo: reabre la app para verlo.")
                    }
                    None => "Base de datos enviada. Reinicia la app para que vuelva a abrirla."
                        .to_string(),
                };
            }
            // El cambio sigue pendiente («Cambios sin push») y el error queda en la barra.
            Err(error) => self.tab.status = format!("Error enviando la base de datos: {error:#}"),
        }
        if let Some(message) = self.tab.queued_push.take() {
            self.push_mobile((!message.is_empty()).then_some(message));
        }
    }

    /// Recoge las tareas terminadas de todas las pestañas, cada una en su propia pestaña.
    fn poll_tasks(&mut self) {
        for (kind, output) in self.tasks.finished() {
            self.finish_task(kind, output);
        }
        for index in 0..self.tabs.len() {
            self.on_tab(index, |app| {
                for (kind, output) in app.tab.tasks.finished() {
                    app.finish_task(kind, output);
                }
            });
        }
    }

    fn finish_task(&mut self, kind: TaskKind, output: Option<TaskOutput>) {
        {
            match output {
                Some(TaskOutput::Devices {
                    mode,
                    fallback,
                    result,
                    elsewhere,
                }) => self.finish_detect_devices(mode, fallback, result, elsewhere),
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
                Some(TaskOutput::LatestRelease(result)) => self.finish_check_for_updates(result),
                Some(TaskOutput::UpdateInstalled { version, result }) => {
                    self.finish_install_update(version, result);
                }
                None => {
                    self.tab.status =
                        format!("Error inesperado: «{}» se interrumpió.", kind.label());
                }
            }
        }
    }

    /// Registra un cambio ya aplicado en la copia local. Con un archivo local el cambio ya está
    /// en el origen; con un dispositivo se envía al momento si el push automático está activo.
    fn record_change(&mut self, message: String) {
        // Puede ser DDL (ALTER, CREATE INDEX…): el esquema se vuelve a leer al mostrarlo.
        self.tab.schema = None;
        if self.tab.target.is_none() || self.tab.db_path.is_none() {
            self.tab.status = message;
            return;
        }
        self.tab.dirty = true;
        self.tab.change_revision += 1;
        if self.settings.auto_push {
            self.push_mobile(Some(message));
        } else {
            self.tab.status = format!("{message} Pulsa «Push al emulador» para enviarlo.");
        }
    }

    fn connect(&mut self, path: PathBuf, origin: String) -> bool {
        let conn = match db::open(&path) {
            Ok(conn) => conn,
            Err(error) => {
                self.tab.status = format!("Error abriendo SQLite: {error:#}");
                return false;
            }
        };

        let objects = match db::list_objects(&conn) {
            Ok(objects) => objects,
            Err(error) => {
                self.tab.status = format!("Error leyendo el esquema: {error:#}");
                return false;
            }
        };

        self.tab.db_path = Some(path);
        self.tab.objects = objects;
        self.tab.editing = None;
        self.tab.dirty = false;
        self.tab.schema = None;
        // Un push en cola era para la copia anterior.
        self.tab.queued_push = None;
        if self.tab.sql.trim().is_empty() {
            self.tab.sql = DEFAULT_SQL.to_string();
            self.tab.generated_sql = None;
        }
        self.tab.status = format!("Conectado: {origin}");
        self.tab.origin = Some(origin);
        if self
            .tab
            .baseline
            .as_ref()
            .is_some_and(|baseline| Some(&baseline.source) != self.tab.db_path.as_ref())
        {
            self.tab.baseline = None;
            self.tab.diff = None;
        } else if self.tab.diff.is_some() {
            // Live Watch o Refrescar: la vista de cambios se mantiene al día.
            self.compare_with_baseline();
        }
        // Reconectar (Live Watch, Refrescar) no debe repetir un DELETE o UPDATE del editor.
        if db::is_read_only(&conn, &self.tab.sql) {
            self.run_sql();
        } else {
            self.tab.result = QueryResult::default();
        }
        true
    }

    fn set_baseline(&mut self) {
        let Some(source) = self.tab.db_path.clone() else {
            return;
        };
        if self.tab.tasks.is_running(TaskKind::Pull) {
            self.tab.status = "Espera a que termine la extracción en curso.".to_string();
            return;
        }
        let copy = std::env::temp_dir()
            .join("db-bridge")
            .join("baseline")
            .join(std::process::id().to_string())
            .join(self.tab.id.to_string())
            .join(source.file_name().unwrap_or_default());
        match diff::snapshot(&source, &copy) {
            Ok(()) => {
                self.tab.baseline = Some(Baseline {
                    source,
                    copy,
                    taken_at: Instant::now(),
                });
                self.tab.status = if self.tab.target.is_some() {
                    "Línea base fijada. Usa la app: los cambios aparecerán en «Ver cambios» con Live Watch o al pulsar Refrescar."
                } else {
                    "Línea base fijada. Los cambios en el archivo aparecerán en «Ver cambios»."
                }
                .to_string();
                if self.tab.diff.is_some() {
                    self.compare_with_baseline();
                }
            }
            Err(error) => self.tab.status = format!("No se pudo fijar la línea base: {error:#}"),
        }
    }

    fn compare_with_baseline(&mut self) {
        let (Some(current), Some(baseline)) = (&self.tab.db_path, &self.tab.baseline) else {
            return;
        };
        match diff::compare(current, &baseline.copy) {
            Ok(result) => {
                self.tab.editing = None;
                self.tab.diff = Some(result);
            }
            Err(error) => {
                self.tab.diff = None;
                self.tab.status = format!("No se pudo comparar con la línea base: {error:#}");
            }
        }
    }

    fn clear_baseline(&mut self) {
        self.tab.baseline = None;
        self.tab.diff = None;
        self.tab.status = "Línea base descartada.".to_string();
    }

    fn select_object(&mut self, object: DbObject) {
        self.tab.selected = Some(object);
        self.tab.diff = None;
        self.tab.view = db::TableView::default();
        self.go_to_page(0);
    }

    fn go_to_page(&mut self, page: usize) {
        self.tab.view.page = page;
        self.load_view();
    }

    /// Clic en la cabecera de una columna: ascendente → descendente → sin orden.
    fn toggle_sort(&mut self, column: &str) {
        self.tab.view.sort = match &self.tab.view.sort {
            Some((current, false)) if current == column => Some((column.to_string(), true)),
            Some((current, true)) if current == column => None,
            _ => Some((column.to_string(), false)),
        };
        self.tab.view.page = 0;
        self.load_view();
    }

    /// Ejecuta la consulta de la vista de tabla actual y la muestra en el editor.
    fn load_view(&mut self) {
        let (Some(object), Some(path)) = (&self.tab.selected, &self.tab.db_path) else {
            return;
        };
        if let Ok(conn) = db::open(path) {
            self.tab.sql = db::table_page_sql(&conn, object, &self.tab.view);
        }
        self.tab.generated_sql = Some(self.tab.sql.clone());
        self.tab.editing = None;
        self.run_sql();
    }

    fn remember_query(&mut self) {
        let sql = self.tab.sql.trim().to_string();
        if sql.is_empty() || sql == DEFAULT_SQL {
            return;
        }
        let history = &mut self.settings.history;
        if history.first() == Some(&sql) {
            return;
        }
        history.retain(|previous| *previous != sql);
        history.insert(0, sql);
        history.truncate(MAX_HISTORY);
        self.persist();
    }

    fn explain(&mut self) {
        let Some(path) = self.tab.db_path.clone() else {
            return;
        };
        if self.tab.tasks.is_running(TaskKind::Pull) {
            self.tab.status = "Espera a que termine la extracción en curso.".to_string();
            return;
        }
        match db::open(&path).and_then(|conn| db::query_plan(&conn, &self.tab.sql)) {
            Ok(plan) => {
                self.tab.status = format!("Plan de consulta · {} pasos", plan.rows.len());
                self.tab.result = plan;
                self.tab.results_tab = ResultsTab::Data;
                self.tab.editing = None;
            }
            Err(error) => self.tab.status = format!("Error SQL: {error:#}"),
        }
    }

    fn refresh(&mut self) {
        if self.tab.db_path.is_none() {
            self.tab.status = "No hay ninguna base de datos conectada.".to_string();
            return;
        }
        match self.tab.target.clone() {
            Some(target) => self.pull_mobile(target),
            None => {
                let read_only = self
                    .tab
                    .db_path
                    .as_ref()
                    .and_then(|path| db::open(path).ok())
                    .map(|conn| db::is_read_only(&conn, &self.tab.sql))
                    .unwrap_or(true);
                if read_only {
                    self.run_sql();
                } else {
                    self.tab.status =
                        "La consulta del editor modifica datos: pulsa Ejecutar para repetirla."
                            .to_string();
                }
            }
        }
    }

    fn run_sql(&mut self) {
        let Some(path) = self.tab.db_path.clone() else {
            self.tab.status = "No hay ninguna base de datos conectada.".to_string();
            return;
        };
        // La copia local se está sobrescribiendo: se ejecutará al conectar si es de lectura.
        if self.tab.tasks.is_running(TaskKind::Pull) {
            self.tab.status = "Espera a que termine la extracción en curso.".to_string();
            return;
        }

        let conn = match db::open(&path) {
            Ok(conn) => conn,
            Err(error) => {
                self.tab.status = format!("Error abriendo la base de datos: {error:#}");
                return;
            }
        };

        let is_generated = self
            .tab
            .generated_sql
            .as_ref()
            .map(|generated| generated.trim() == self.tab.sql.trim())
            .unwrap_or(false);

        let outcome = if is_generated {
            self.tab
                .selected
                .as_ref()
                .map(|object| db::run_table_query(&conn, object, db::ROW_LIMIT, &self.tab.view))
                .unwrap_or_else(|| db::run_script(&conn, &self.tab.sql))
        } else {
            db::run_script(&conn, &self.tab.sql)
        };
        drop(conn);

        match outcome {
            Ok(result) => {
                // La tabla encogió (Live Watch, Refrescar) y la página ya no existe.
                if let Some(total) = result.total {
                    let last_page = last_page(total);
                    if self.tab.view.page > last_page {
                        self.go_to_page(last_page);
                        return;
                    }
                }
                let rows = result.rows.len();
                let elapsed = result.elapsed.as_millis();
                let modified = result.modified;
                let statements = result.statements;
                self.tab.result = result;
                if !is_generated {
                    self.remember_query();
                }
                let executed = if statements > 1 {
                    format!("{statements} sentencias ejecutadas")
                } else {
                    "Sentencia ejecutada".to_string()
                };
                if modified {
                    self.record_change(format!("{executed} ({elapsed} ms)."));
                } else if statements > 1 {
                    self.tab.status = format!("{executed} · {rows} filas · {elapsed} ms");
                } else {
                    self.tab.status = format!("{rows} filas · {elapsed} ms");
                }
            }
            Err(error) => {
                self.tab.result = QueryResult::default();
                self.tab.status = format!("Error SQL: {error:#}");
            }
        }
    }

    /// Contexto para modificar una fila de la tabla seleccionada: conexión, tabla, expresión de
    /// la clave y valor de la clave de `row`. Deja el motivo en la barra si no se puede.
    fn row_target(&mut self, row: usize) -> Option<(rusqlite::Connection, String, String, Value)> {
        let path = self.tab.db_path.clone()?;
        if self.tab.tasks.is_running(TaskKind::Pull) {
            self.tab.status =
                "Cambio descartado: la base de datos se está volviendo a extraer.".to_string();
            return None;
        }
        let (Some(object), Some(key_expr)) =
            (self.tab.selected.clone(), self.tab.result.key_expr.clone())
        else {
            self.tab.status = "La vista actual no permite editar filas.".to_string();
            return None;
        };
        let Some(key) = self.tab.result.keys.get(row).cloned() else {
            self.tab.status = "No se encontró la clave de la fila.".to_string();
            return None;
        };
        match db::open(&path) {
            Ok(conn) => Some((conn, object.name, key_expr, key)),
            Err(error) => {
                self.tab.status = format!("Error abriendo la base de datos: {error:#}");
                None
            }
        }
    }

    fn apply_edit(&mut self, row: usize, column: usize, new_text: String) {
        let Some(current) = self
            .tab
            .result
            .rows
            .get(row)
            .and_then(|row| row.get(column))
        else {
            return;
        };
        // Abrir y cerrar el editor sin tocar nada no debe escribir (ni enviar al dispositivo).
        let unchanged = match current {
            Value::Null => new_text.is_empty() || new_text.eq_ignore_ascii_case("NULL"),
            value => db::value_display(value) == new_text,
        };
        if unchanged {
            self.tab.status = "Sin cambios.".to_string();
            return;
        }
        let new_value = if new_text.eq_ignore_ascii_case("NULL") {
            Value::Null
        } else {
            Value::Text(new_text)
        };
        self.update_value(row, column, new_value);
    }

    fn update_value(&mut self, row: usize, column: usize, new_value: Value) {
        let Some(column_name) = self.tab.result.columns.get(column).cloned() else {
            self.tab.status = "No se encontró la columna.".to_string();
            return;
        };
        let Some((conn, table, key_expr, key)) = self.row_target(row) else {
            return;
        };
        match db::update_cell(&conn, &table, &key_expr, &key, &column_name, &new_value) {
            Ok(1) => {
                if let Some(value) = self
                    .tab
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
                self.tab.status = "La actualización no afectó ninguna fila.".to_string();
            }
            Err(error) => {
                self.tab.status = format!("Error actualizando la celda: {error:#}");
            }
        }
    }

    fn delete_row(&mut self, row: usize) {
        let Some((conn, table, key_expr, key)) = self.row_target(row) else {
            return;
        };
        match db::delete_row(&conn, &table, &key_expr, &key) {
            Ok(0) => self.tab.status = "La fila ya no existe.".to_string(),
            Ok(_) => {
                drop(conn);
                self.run_sql();
                self.record_change(format!("Fila eliminada de «{table}»."));
            }
            Err(error) => self.tab.status = format!("Error eliminando la fila: {error:#}"),
        }
    }

    fn open_insert_form(&mut self) {
        let (Some(path), Some(object)) = (self.tab.db_path.clone(), self.tab.selected.clone())
        else {
            return;
        };
        let schema = db::open(&path).and_then(|conn| db::table_schema(&conn, &object.name));
        let schema = match schema {
            Ok(schema) => schema,
            Err(error) => {
                self.tab.status = format!("No se pudo leer el esquema: {error:#}");
                return;
            }
        };
        let primary_keys = schema
            .columns
            .iter()
            .filter(|column| column.primary_key > 0)
            .count();
        let fields = schema
            .columns
            .into_iter()
            .map(|column| {
                // INTEGER PRIMARY KEY es el rowid: SQLite lo asigna si se deja por defecto.
                let rowid_alias = !object.without_rowid
                    && primary_keys == 1
                    && column.primary_key == 1
                    && column.decl_type.eq_ignore_ascii_case("INTEGER");
                let required = column.not_null && column.default.is_none() && !rowid_alias;
                InsertField {
                    mode: if required {
                        FieldMode::Value
                    } else {
                        FieldMode::Default
                    },
                    column,
                    text: String::new(),
                }
            })
            .collect();
        self.tab.insert_form = Some(InsertForm {
            table: object.name,
            fields,
            error: None,
        });
    }

    fn submit_insert(&mut self) {
        let Some(path) = self.tab.db_path.clone() else {
            return;
        };
        let Some(form) = &mut self.tab.insert_form else {
            return;
        };
        if self.tab.tasks.is_running(TaskKind::Pull) {
            form.error = Some("Espera a que termine la extracción en curso.".to_string());
            return;
        }
        let values: Vec<(String, Value)> = form
            .fields
            .iter()
            .filter_map(|field| {
                let value = match field.mode {
                    FieldMode::Default => return None,
                    FieldMode::Null => Value::Null,
                    FieldMode::Value => Value::Text(field.text.clone()),
                };
                Some((field.column.name.clone(), value))
            })
            .collect();
        let outcome = db::open(&path).and_then(|conn| db::insert_row(&conn, &form.table, &values));
        match outcome {
            Ok(_) => {
                let table = form.table.clone();
                self.tab.insert_form = None;
                self.run_sql();
                self.record_change(format!("Fila insertada en «{table}»."));
            }
            Err(error) => form.error = Some(format!("{error:#}")),
        }
    }

    fn open_viewer(&mut self, row: usize, column: usize) {
        let Some(value) = self
            .tab
            .result
            .rows
            .get(row)
            .and_then(|row| row.get(column))
        else {
            return;
        };
        let column_name = self
            .tab
            .result
            .columns
            .get(column)
            .cloned()
            .unwrap_or_default();
        let (summary, body, blob) = match value {
            Value::Null => ("NULL".to_string(), "NULL".to_string(), None),
            Value::Integer(number) => ("Entero".to_string(), number.to_string(), None),
            Value::Real(number) => ("Real".to_string(), number.to_string(), None),
            Value::Text(text) => {
                let size = db::format_size(text.len() as u64);
                match value_view::pretty_json(text) {
                    Some(json) => (format!("Texto · {size} · JSON"), json, None),
                    None => (format!("Texto · {size}"), text.clone(), None),
                }
            }
            Value::Blob(bytes) => {
                let mut summary = format!("BLOB · {}", db::format_size(bytes.len() as u64));
                if let Some(kind) = value_view::blob_kind(bytes) {
                    summary.push_str(&format!(" · {kind}"));
                }
                (summary, value_view::hex_dump(bytes), Some(bytes.clone()))
            }
        };
        self.tab.viewer = Some(ValueViewer {
            title: format!("{column_name} · fila {}", row + 1),
            summary,
            body,
            blob,
        });
    }

    fn apply_cell_action(&mut self, ctx: &egui::Context, action: CellAction) {
        match action {
            CellAction::View(row, column) => self.open_viewer(row, column),
            CellAction::Copy(row, column) => {
                if let Some(value) = self
                    .tab
                    .result
                    .rows
                    .get(row)
                    .and_then(|row| row.get(column))
                {
                    let text = match value {
                        Value::Blob(bytes) => value_view::hex_dump(bytes),
                        value => db::value_display(value),
                    };
                    ctx.copy_text(text);
                    self.tab.status = "Valor copiado al portapapeles.".to_string();
                }
            }
            CellAction::SetNull(row, column) => self.update_value(row, column, Value::Null),
            CellAction::Delete(row) => self.tab.pending_delete = Some(row),
        }
    }

    /// Diálogos modales: visor de valores, formulario de inserción y confirmación de borrado.
    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(index) = self.pending_close {
            let mut confirmed = false;
            let mut cancel = false;
            let tab = self.tab_at(index);
            let detail = if tab.dirty {
                "Tiene cambios sin enviar al dispositivo y se perderán."
            } else {
                "Tiene una transferencia en curso que quedará sin terminar."
            };
            let title = tab.title();
            let response = egui::Modal::new(egui::Id::unique("confirm_close")).show(ctx, |ui| {
                ui.set_width(360.0);
                ui.label(theme::strong(format!("¿Cerrar «{title}»?")));
                ui.add_space(4.0);
                ui.label(RichText::new(detail).size(12.0).color(theme::muted()));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let close =
                        egui::Button::new(theme::strong("Cerrar").color(theme::on_accent()))
                            .fill(theme::danger());
                    confirmed = ui.add(close).clicked();
                    cancel = ui.button("Cancelar").clicked();
                });
            });
            if confirmed {
                self.pending_close = None;
                self.close_tab(index);
            } else if cancel || response.should_close() {
                self.pending_close = None;
            }
        }

        if let Some(viewer) = &self.tab.viewer {
            let mut close = false;
            let mut save = false;
            let response = egui::Modal::new(egui::Id::unique("value_viewer")).show(ctx, |ui| {
                ui.set_width(640.0);
                ui.label(theme::strong(viewer.title.as_str()));
                ui.label(
                    RichText::new(viewer.summary.as_str())
                        .size(12.0)
                        .color(theme::muted()),
                );
                ui.add_space(8.0);
                egui::ScrollArea::both().max_height(420.0).show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut viewer.body.as_str())
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY),
                    );
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Copiar").clicked() {
                        ui.ctx().copy_text(viewer.body.clone());
                    }
                    if viewer.blob.is_some() && ui.button("Guardar como…").clicked() {
                        save = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close = ui.button("Cerrar").clicked();
                    });
                });
            });
            if save && let Some(bytes) = &viewer.blob {
                let name = format!("valor.{}", value_view::blob_extension(bytes));
                if let Some(path) = rfd::FileDialog::new().set_file_name(name).save_file() {
                    self.tab.status = match std::fs::write(&path, bytes) {
                        Ok(()) => format!("BLOB guardado en {}", path.display()),
                        Err(error) => format!("No se pudo guardar el BLOB: {error:#}"),
                    };
                }
            }
            if close || response.should_close() {
                self.tab.viewer = None;
            }
        }

        if let Some(row) = self.tab.pending_delete {
            let mut confirmed = false;
            let mut cancel = false;
            let response = egui::Modal::new(egui::Id::unique("confirm_delete")).show(ctx, |ui| {
                ui.set_width(360.0);
                ui.label(theme::strong(format!("¿Eliminar la fila {}?", row + 1)));
                ui.add_space(4.0);
                ui.label(
                    RichText::new(if self.tab.target.is_some() && self.settings.auto_push {
                        "Se borrará en la copia local y se enviará al dispositivo."
                    } else {
                        "Se borrará de la base de datos."
                    })
                    .size(12.0)
                    .color(theme::muted()),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let delete =
                        egui::Button::new(theme::strong("Eliminar").color(theme::on_accent()))
                            .fill(theme::danger());
                    confirmed = ui.add(delete).clicked();
                    cancel = ui.button("Cancelar").clicked();
                });
            });
            if confirmed {
                self.tab.pending_delete = None;
                self.delete_row(row);
            } else if cancel || response.should_close() {
                self.tab.pending_delete = None;
            }
        }

        if self.tab.insert_form.is_some() {
            let mut submit = false;
            let mut cancel = false;
            let response = egui::Modal::new(egui::Id::unique("insert_form")).show(ctx, |ui| {
                let Some(form) = &mut self.tab.insert_form else {
                    return;
                };
                ui.set_width(560.0);
                ui.label(theme::strong(format!("Nueva fila en «{}»", form.table)));
                ui.label(
                    RichText::new(
                        "«Por defecto» deja que SQLite asigne el valor (DEFAULT, rowid o NULL).",
                    )
                    .size(12.0)
                    .color(theme::subtle()),
                );
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .max_height(380.0)
                    .show(ui, |ui| {
                        egui::Grid::new("insert_grid")
                            .num_columns(3)
                            .spacing(egui::vec2(12.0, 6.0))
                            .show(ui, |ui| {
                                for field in &mut form.fields {
                                    ui.vertical(|ui| {
                                        ui.label(theme::mono(field.column.name.as_str()));
                                        let mut detail = field.column.decl_type.clone();
                                        if field.column.not_null {
                                            detail.push_str(" NOT NULL");
                                        }
                                        if let Some(default) = &field.column.default {
                                            detail.push_str(&format!(" = {default}"));
                                        }
                                        ui.label(
                                            RichText::new(detail).size(11.0).color(theme::subtle()),
                                        );
                                    });
                                    let modes = [
                                        (FieldMode::Default, "Por defecto"),
                                        (FieldMode::Null, "NULL"),
                                        (FieldMode::Value, "Valor"),
                                    ];
                                    theme::segmented(ui, &mut field.mode, &modes);
                                    ui.add_enabled(
                                        field.mode == FieldMode::Value,
                                        egui::TextEdit::singleline(&mut field.text)
                                            .font(egui::TextStyle::Monospace)
                                            .desired_width(200.0),
                                    );
                                    ui.end_row();
                                }
                            });
                    });
                if let Some(error) = &form.error {
                    ui.add_space(6.0);
                    ui.label(RichText::new(error.as_str()).color(theme::danger()));
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    submit = ui.add(theme::primary_button("Insertar")).clicked();
                    cancel = ui.button("Cancelar").clicked();
                });
            });
            if submit {
                self.submit_insert();
            } else if cancel || response.should_close() {
                self.tab.insert_form = None;
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
        let content = db::to_csv(&self.tab.result);
        match std::fs::write(&path, content) {
            Ok(()) => self.tab.status = format!("CSV exportado en {}", path.display()),
            Err(error) => self.tab.status = format!("No se pudo exportar el CSV: {error:#}"),
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
        let content = match serde_json::to_string_pretty(&db::to_json(&self.tab.result)) {
            Ok(content) => content,
            Err(error) => {
                self.tab.status = format!("No se pudo generar el JSON: {error:#}");
                return;
            }
        };
        match std::fs::write(&path, content) {
            Ok(()) => self.tab.status = format!("JSON exportado en {}", path.display()),
            Err(error) => self.tab.status = format!("No se pudo exportar el JSON: {error:#}"),
        }
    }

    fn save_favorite(&mut self) {
        let name = self.new_favorite_name.trim().to_string();
        if name.is_empty() || self.tab.sql.trim().is_empty() {
            self.tab.status = "Escribe un nombre y una consulta antes de guardarla.".to_string();
            return;
        }
        self.settings.favorites.push(FavoriteQuery {
            name,
            sql: self.tab.sql.clone(),
        });
        self.new_favorite_name.clear();
        self.persist();
        self.tab.status = "Consulta guardada en favoritos.".to_string();
    }

    /// Consulta la última Release en segundo plano si está activado y toca (o si `force`).
    fn check_for_updates(&mut self, force: bool) {
        if !self.settings.check_updates || self.tasks.is_running(TaskKind::CheckUpdate) {
            return;
        }
        if !force
            && !update_check::is_due(self.settings.last_update_check, update_check::now_secs())
        {
            return;
        }
        self.tasks.spawn(TaskKind::CheckUpdate, || {
            TaskOutput::LatestRelease(update_check::fetch_latest())
        });
    }

    /// Sin red o con GitHub caído no se avisa de nada: se reintenta en el próximo arranque.
    fn finish_check_for_updates(&mut self, result: anyhow::Result<Release>) {
        if let Ok(release) = result {
            self.settings.last_update_check = update_check::now_secs();
            self.settings.latest_release = Some(release);
            self.persist();
        }
    }

    /// Release que merece aviso: más nueva que la instalada y no omitida por el usuario.
    fn available_update(&self) -> Option<&Release> {
        self.settings.latest_release.as_ref().filter(|release| {
            self.settings.check_updates
                && release.version != self.settings.skipped_version
                && update_check::is_newer(&release.version, update_check::CURRENT_VERSION)
        })
    }

    /// Descarga e instala `version` en segundo plano; se aplica al reiniciar.
    fn install_update(&mut self, version: String) {
        let Ok(installation) = self.installation.clone() else {
            return;
        };
        if self.tasks.is_running(TaskKind::InstallUpdate) {
            return;
        }
        self.update_install = None;
        self.tasks.spawn(TaskKind::InstallUpdate, move || {
            let result = updater::install(&installation, &version);
            TaskOutput::UpdateInstalled { version, result }
        });
    }

    fn finish_install_update(&mut self, version: String, result: anyhow::Result<()>) {
        match result {
            Ok(()) => {
                self.tab.status = format!("DB Bridge {version} instalado: reinicia para usarlo.");
                self.update_install = Some(UpdateInstall::Installed(version));
            }
            Err(error) => {
                self.tab.status = format!("No se pudo actualizar: {error:#}");
                self.update_install = Some(UpdateInstall::Failed(format!("{error:#}")));
            }
        }
    }

    /// Abre la versión instalada y cierra esta ventana.
    fn restart_into_update(&mut self, ctx: &egui::Context) {
        let Ok(installation) = &self.installation else {
            return;
        };
        match installation.relaunch() {
            Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(error) => {
                self.tab.status = format!("No se pudo reiniciar: {error:#}");
                self.update_install = Some(UpdateInstall::Failed(format!("{error:#}")));
            }
        }
    }

    fn update_banner(&mut self, ui: &mut egui::Ui) {
        let Some(release) = self.available_update().cloned() else {
            return;
        };
        let installing = self.tasks.is_running(TaskKind::InstallUpdate);
        let installed = matches!(
            &self.update_install,
            Some(UpdateInstall::Installed(version)) if *version == release.version
        );
        let mut install = false;
        let mut restart = false;
        let frame = Frame::NONE
            .fill(theme::raised())
            .inner_margin(Margin::symmetric(16, 6));
        egui::Panel::top("update_banner")
            .frame(frame)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let message = if installing {
                        ui.add(egui::Spinner::new().size(12.0).color(theme::accent()));
                        format!("Descargando e instalando DB Bridge {}…", release.version)
                    } else if installed {
                        theme::dot(ui, theme::success(), 3.5);
                        format!(
                            "DB Bridge {} está instalado. Reinicia para usarlo.",
                            release.version
                        )
                    } else {
                        theme::dot(ui, theme::accent(), 3.5);
                        format!(
                            "Hay una versión nueva de DB Bridge: {} (tienes la {}).",
                            release.version,
                            update_check::CURRENT_VERSION
                        )
                    };
                    ui.label(RichText::new(message).size(12.5).color(theme::text()));
                    if !installing && let Some(UpdateInstall::Failed(error)) = &self.update_install
                    {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("No se pudo actualizar: {error}"))
                                    .size(12.0)
                                    .color(theme::warning()),
                            )
                            .truncate(),
                        );
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if installed {
                            let busy = (0..self.tabs.len())
                                .any(|index| self.tab_at(index).has_pending_work());
                            restart = ui
                                .add_enabled(!busy, theme::primary_button("Reiniciar ahora"))
                                .on_disabled_hover_text(
                                    "Envía los cambios pendientes al dispositivo antes de \
                                     reiniciar.",
                                )
                                .clicked();
                            return;
                        }
                        if installing {
                            return;
                        }
                        if ui
                            .button("Omitir esta versión")
                            .on_hover_text("No volver a avisar hasta que salga otra.")
                            .clicked()
                        {
                            self.settings.skipped_version = release.version.clone();
                            self.persist();
                        }
                        if ui
                            .button("Novedades")
                            .on_hover_text("Abre las notas de la versión en GitHub.")
                            .clicked()
                        {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(&release.url));
                        }
                        let download = match &self.installation {
                            Ok(_) => egui::Button::new("Descargar"),
                            Err(_) => theme::primary_button("Descargar"),
                        };
                        let hover = match &self.installation {
                            Ok(_) => "Abre la página de descarga.".to_string(),
                            Err(reason) => format!(
                                "Abre la página de descarga. No se puede actualizar desde la \
                                 app: {reason}."
                            ),
                        };
                        if ui.add(download).on_hover_text(hover).clicked() {
                            ui.ctx()
                                .open_url(egui::OpenUrl::new_tab(update_check::DOWNLOAD_URL));
                        }
                        if self.installation.is_ok() {
                            install = ui
                                .add(theme::primary_button("Actualizar"))
                                .on_hover_text(
                                    "Descarga la versión nueva, comprueba su SHA-256 y \
                                     sustituye esta instalación. Se aplica al reiniciar.",
                                )
                                .clicked();
                        }
                    });
                });
            });
        if install {
            self.install_update(release.version);
        }
        if restart {
            self.restart_into_update(ui.ctx());
        }
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
        let interval = self.settings.live_watch_secs;
        if interval == 0 {
            return;
        }
        if self.tab.last_refresh.elapsed() >= Duration::from_secs(interval)
            && self.tab.editing.is_none()
            // Los diálogos guardan índices de fila: no se refresca por debajo de ellos.
            && self.tab.pending_delete.is_none()
            && self.tab.insert_form.is_none()
            && !self.tab.dirty
            && self.tab.db_path.is_some()
            && !self.transfer_running()
        {
            self.tab.last_refresh = Instant::now();
            self.refresh();
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn status_color(&self) -> Color32 {
        let status = self.tab.status.as_str();
        if status.starts_with("Error") || status.starts_with("No se pudo") {
            theme::danger()
        } else if self.tab.db_path.is_some() {
            theme::success()
        } else {
            theme::faint()
        }
    }

    fn top_panel(&mut self, ui: &mut egui::Ui) {
        let frame = Frame::NONE
            .fill(theme::surface())
            .inner_margin(Margin::symmetric(16, 10));
        egui::Panel::top("top_panel").frame(frame).show(ui, |ui| {
            ui.horizontal(|ui| {
                theme::dot(ui, theme::accent(), 5.0);
                ui.label(theme::strong("DB Bridge").size(14.5));
                ui.add_space(12.0);

                let modes = [SourceMode::Local, SourceMode::Ios, SourceMode::Android]
                    .map(|mode| (mode, mode.label()));
                let mut mode = self.tab.mode;
                if theme::segmented(ui, &mut mode, &modes) {
                    self.change_mode(mode);
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let themes = [
                        (ThemeMode::Dark, "Oscuro"),
                        (ThemeMode::Light, "Claro"),
                        (ThemeMode::Auto, "Auto"),
                    ];
                    let mut mode = self.settings.theme;
                    // En un layout de derecha a izquierda las opciones se dibujan al revés.
                    if theme::segmented(ui, &mut mode, &themes) {
                        self.settings.theme = mode;
                        theme::set_mode(ui.ctx(), mode);
                        self.persist();
                    }
                    ui.add_space(4.0);

                    let transfer_idle = !self.transfer_running();
                    if self.tab.db_path.is_some()
                        && self.tab.target.is_some()
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
                    if self.tab.mode != SourceMode::Local
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
                });
            });

            ui.add_space(8.0);

            ui.horizontal_wrapped(|ui| match self.tab.mode {
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
                    if !self.tab.detected_devices.is_empty() {
                        theme::field(ui, "Dispositivo", |ui| {
                            let selected = self
                                .tab
                                .detected_devices
                                .iter()
                                .find(|device| device.id == self.tab.device)
                                .map_or(self.tab.device.as_str(), |device| device.name.as_str());
                            egui::ComboBox::from_id_salt("device_selector")
                                .selected_text(selected)
                                .width(200.0)
                                .truncate()
                                .show_ui(ui, |ui| {
                                    for device in &self.tab.detected_devices {
                                        let current = device.id == self.tab.device;
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
                                .on_hover_text(self.tab.device.as_str());
                        });
                    }
                    if let Some(device) = chosen_device {
                        self.select_device(device);
                    }

                    let label = if self.tab.mode == SourceMode::Ios {
                        "Bundle ID"
                    } else {
                        "Package"
                    };
                    let mut app_changed = false;
                    theme::field(ui, label, |ui| {
                        if self.tab.detected_apps.is_empty() {
                            let response = ui.add(
                                egui::TextEdit::singleline(&mut self.tab.app_id)
                                    .frame(Frame::NONE)
                                    .font(egui::TextStyle::Monospace)
                                    .desired_width(220.0)
                                    .align(egui::Align2::LEFT_CENTER)
                                    .hint_text("com.miempresa.app"),
                            );
                            if response.changed() {
                                self.tab.detected_dbs.clear();
                            }
                        } else {
                            egui::ComboBox::from_id_salt("app_selector")
                                .selected_text(if self.tab.app_id.is_empty() {
                                    RichText::new("Selecciona una app").color(theme::subtle())
                                } else {
                                    theme::mono(self.tab.app_id.as_str())
                                })
                                .width(240.0)
                                .show_ui(ui, |ui| {
                                    for app in &self.tab.detected_apps {
                                        if ui
                                            .selectable_value(
                                                &mut self.tab.app_id,
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
                        self.tab.connect_after_detect = true;
                        self.detect_databases();
                    }

                    let mut db_changed = false;
                    theme::field(ui, "Archivo", |ui| {
                        if self.tab.detected_dbs.is_empty() {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.tab.db_name)
                                    .frame(Frame::NONE)
                                    .font(egui::TextStyle::Monospace)
                                    .desired_width(260.0)
                                    .align(egui::Align2::LEFT_CENTER)
                                    .hint_text("offline.db"),
                            );
                        } else {
                            egui::ComboBox::from_id_salt("db_selector")
                                .selected_text(theme::mono(self.tab.db_name.as_str()))
                                .width(260.0)
                                .truncate()
                                .show_ui(ui, |ui| {
                                    for database in &self.tab.detected_dbs {
                                        db_changed |= ui
                                            .selectable_value(
                                                &mut self.tab.db_name,
                                                database.clone(),
                                                theme::mono(database),
                                            )
                                            .changed();
                                    }
                                })
                                .response
                                .on_hover_text(self.tab.db_name.as_str());
                        }
                    });
                    if db_changed {
                        self.connect_selection();
                    }

                    let detecting = self.tab.tasks.is_running(TaskKind::DetectDevices)
                        || self.tab.tasks.is_running(TaskKind::DetectApps)
                        || self.tab.tasks.is_running(TaskKind::DetectDatabases);
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

            let mut switch_to = None;
            if let Some(notice) = &self.tab.detect_notice
                && self.tab.mode != SourceMode::Local
            {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    theme::dot(ui, theme::warning(), 3.5);
                    ui.label(
                        RichText::new(notice.message.as_str())
                            .size(12.5)
                            .color(theme::warning()),
                    );
                    if let Some(mode) = notice.switch_to
                        && ui.button(format!("Cambiar a {}", mode.label())).clicked()
                    {
                        switch_to = Some(mode);
                    }
                });
            }
            if let Some(mode) = switch_to {
                self.change_mode(mode);
            }
        });
    }

    fn status_panel(&mut self, ui: &mut egui::Ui) {
        let frame = Frame::NONE
            .fill(theme::surface())
            .inner_margin(Margin::symmetric(16, 5));
        egui::Panel::bottom("status_panel")
            .frame(frame)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(kind) = self.tab.tasks.running().chain(self.tasks.running()).last()
                    {
                        ui.add(egui::Spinner::new().size(12.0).color(theme::accent()));
                        ui.label(RichText::new(kind.label()).size(12.0).color(theme::text()));
                        ui.add_space(8.0);
                    }
                    theme::dot(ui, self.status_color(), 3.5);
                    ui.add(
                        egui::Label::new(
                            RichText::new(&self.tab.status)
                                .size(12.0)
                                .color(theme::muted()),
                        )
                        .truncate(),
                    );
                    if self.tab.dirty {
                        ui.add_space(8.0);
                        theme::dot(ui, theme::warning(), 3.5);
                        ui.label(
                            RichText::new("Cambios sin push")
                                .size(12.0)
                                .color(theme::warning()),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.version_menu(ui);
                        if let Some(path) = &self.tab.db_path {
                            ui.label(
                                theme::mono(path.display().to_string())
                                    .size(11.5)
                                    .color(theme::subtle()),
                            );
                        }
                    });
                });
            });
    }

    fn version_menu(&mut self, ui: &mut egui::Ui) {
        let label = RichText::new(format!("v{}", update_check::CURRENT_VERSION))
            .size(11.5)
            .color(theme::subtle());
        ui.menu_button(label, |ui| {
            if ui
                .checkbox(
                    &mut self.settings.check_updates,
                    "Buscar actualizaciones al iniciar",
                )
                .on_hover_text(
                    "Consulta GitHub como mucho una vez al día. No se envía ningún dato.",
                )
                .changed()
            {
                self.persist();
                self.check_for_updates(false);
            }
            let checking = self.tasks.is_running(TaskKind::CheckUpdate);
            if ui
                .add_enabled(
                    self.settings.check_updates && !checking,
                    egui::Button::new("Buscar ahora"),
                )
                .clicked()
            {
                // Al buscar a mano se vuelve a avisar aunque se hubiera omitido la versión.
                self.settings.skipped_version.clear();
                self.check_for_updates(true);
            }
            match &self.settings.latest_release {
                Some(release)
                    if update_check::is_newer(&release.version, update_check::CURRENT_VERSION) =>
                {
                    if ui
                        .button(format!("Descargar la {}", release.version))
                        .clicked()
                    {
                        ui.ctx()
                            .open_url(egui::OpenUrl::new_tab(update_check::DOWNLOAD_URL));
                    }
                }
                Some(_) => {
                    ui.weak("Tienes la última versión.");
                }
                None => {}
            }
        });
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        let frame = Frame::NONE
            .fill(theme::sidebar())
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
                    if !self.tab.objects.is_empty() {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(self.tab.objects.len().to_string())
                                    .size(11.5)
                                    .color(theme::subtle()),
                            );
                        });
                    }
                });
                ui.add(
                    egui::TextEdit::singleline(&mut self.tab.table_filter)
                        .hint_text("Filtrar tablas")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(2.0);

                egui::ScrollArea::vertical()
                    .id_salt("objects_scroll")
                    .max_height(320.0)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        if self.tab.objects.is_empty() {
                            ui.weak("Sin conexión.");
                        }
                        let filter = self.tab.table_filter.trim().to_lowercase();
                        for object in self.tab.objects.clone() {
                            if !filter.is_empty() && !object.name.to_lowercase().contains(&filter) {
                                continue;
                            }
                            let selected = self
                                .tab
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
                                theme::text()
                            } else {
                                theme::text_soft()
                            };
                            let button = egui::Button::selectable(
                                selected,
                                theme::mono(object.name.as_str()).color(name_color),
                            )
                            .right_text(RichText::new(tag).size(11.0).color(theme::subtle()))
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
                                        .color(theme::text_soft()),
                                )
                                .right_text("")
                                .min_size(egui::vec2(width, 28.0));
                                if ui.add(button).on_hover_text(&favorite.sql).clicked() {
                                    self.tab.sql = favorite.sql.clone();
                                    self.tab.generated_sql = None;
                                    self.tab.selected = None;
                                    self.tab.diff = None;
                                    self.tab.editing = None;
                                    self.run_sql();
                                }
                                let delete = egui::Button::new(
                                    RichText::new("×").size(15.0).color(theme::subtle()),
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
            match &self.tab.db_path {
                Some(path) => {
                    ui.horizontal(|ui| {
                        theme::dot(ui, theme::success(), 4.0);
                        ui.label(theme::strong(self.tab.mode.label()));
                    });
                    if let Some(origin) = &self.tab.origin {
                        ui.add(
                            egui::Label::new(theme::mono(origin.as_str()).color(theme::muted()))
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
                        ui.label(RichText::new(text).size(12.0).color(theme::subtle()))
                            .on_hover_text(detail);
                    }
                    ui.add_space(6.0);
                    self.baseline_controls(ui);
                }
                None => {
                    ui.horizontal(|ui| {
                        theme::dot(ui, theme::faint(), 4.0);
                        ui.label(theme::strong("Sin conexión"));
                    });
                    ui.label(
                        RichText::new("Abre un archivo o extrae la BD de un dispositivo.")
                            .size(12.0)
                            .color(theme::subtle()),
                    );
                }
            }
        });
    }

    fn baseline_controls(&mut self, ui: &mut egui::Ui) {
        let pulling = self.tab.tasks.is_running(TaskKind::Pull);
        match &self.tab.baseline {
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
                    theme::dot(ui, theme::accent(), 3.5);
                    ui.label(
                        RichText::new(format!("Línea base · {age}"))
                            .size(12.0)
                            .color(theme::muted()),
                    );
                });
                ui.horizontal(|ui| {
                    let label = if self.tab.diff.is_some() {
                        "Ocultar cambios"
                    } else {
                        "Ver cambios"
                    };
                    if ui.add_enabled(!pulling, egui::Button::new(label)).clicked() {
                        if self.tab.diff.is_some() {
                            self.tab.diff = None;
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
                        egui::Button::new(RichText::new("×").size(15.0).color(theme::subtle()))
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
        let Some(diff) = self.tab.diff.take() else {
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
                        if let Some(baseline) = &self.tab.baseline {
                            ui.label(
                                RichText::new(format_age(baseline.taken_at.elapsed()))
                                    .color(theme::muted()),
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
                            ui.add_space(8.0);
                            // De derecha a izquierda: el selector queda a la izquierda de los botones.
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    self.live_watch_selector(ui);
                                },
                            );
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
                                    .color(theme::text_soft()),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(if self.tab.target.is_some() {
                                    "Usa la app: con Live Watch activo los cambios aparecen solos."
                                } else {
                                    "Modifica el archivo y pulsa Recalcular."
                                })
                                .size(12.0)
                                .color(theme::subtle()),
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
            self.tab.diff = Some(diff);
        }
        if recompute {
            self.compare_with_baseline();
        }
    }

    fn central_panel(&mut self, ui: &mut egui::Ui) {
        let frame = Frame::NONE.fill(theme::bg()).inner_margin(Margin::same(16));
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            if self.tab.db_path.is_none() {
                self.welcome(ui);
                return;
            }
            if self.tab.diff.is_some() {
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
                                    .color(theme::text()),
                            );
                            ui.add_space(2.0);
                            ui.label(
                                RichText::new(
                                    "Abre un archivo SQLite local o extrae la base de datos de tu app desde el simulador o el emulador.",
                                )
                                .size(14.0)
                                .color(theme::muted()),
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
                                    let active = self.tab.mode == mode;
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
                                Stroke::new(1.5, theme::accent())
                            } else {
                                Stroke::new(1.5, theme::border_strong())
                            };
                            Frame::NONE
                                .fill(theme::sidebar())
                                .stroke(drop_stroke)
                                .corner_radius(CornerRadius::same(12))
                                .inner_margin(Margin::same(28))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.vertical_centered(|ui| {
                                        ui.label(
                                            RichText::new("Arrastra un archivo .db aquí")
                                                .size(14.5)
                                                .color(theme::text()),
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
                                        RichText::new(tag).size(11.5).color(theme::muted()),
                                        theme::mono(recent.label()).color(theme::text()),
                                    ))
                                    .right_text(
                                        RichText::new("Reconectar").size(12.0).color(theme::subtle()),
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
        if self.tab.mode != mode {
            self.tab.mode = mode;
            self.tab.detected_apps.clear();
            self.tab.detected_dbs.clear();
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
                                .color(theme::on_accent().gamma_multiply(0.7)),
                        );
                        if ui.add(run).clicked() {
                            self.run_sql();
                        }
                        if ui
                            .add_enabled(self.tab.db_path.is_some(), egui::Button::new("Refrescar"))
                            .clicked()
                        {
                            self.refresh();
                        }
                        if ui
                            .add_enabled(self.tab.db_path.is_some(), egui::Button::new("Plan"))
                            .on_hover_text("EXPLAIN QUERY PLAN de la consulta del editor.")
                            .clicked()
                        {
                            self.explain();
                        }
                        let mut chosen = None;
                        ui.add_enabled_ui(!self.settings.history.is_empty(), |ui| {
                            ui.menu_button("Historial", |ui| {
                                ui.set_max_width(520.0);
                                for sql in &self.settings.history {
                                    let first_line = sql.lines().next().unwrap_or_default();
                                    let label = value_view::cell_preview(sql)
                                        .unwrap_or_else(|| first_line.to_string());
                                    if ui
                                        .add(egui::Button::new(theme::mono(label)).truncate())
                                        .on_hover_text(theme::mono(sql.as_str()))
                                        .clicked()
                                    {
                                        chosen = Some(sql.clone());
                                        ui.close();
                                    }
                                }
                            });
                        });
                        if let Some(sql) = chosen {
                            self.tab.sql = sql;
                            self.tab.generated_sql = None;
                            self.tab.selected = None;
                            self.tab.editing = None;
                            self.tab.status =
                                "Consulta del historial cargada en el editor.".to_string();
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Limpiar").clicked() {
                                self.tab.result = QueryResult::default();
                                self.tab.selected = None;
                                self.tab.generated_sql = None;
                                self.tab.editing = None;
                                self.tab.sql = DEFAULT_SQL.to_string();
                                self.tab.status = "Vista limpiada.".to_string();
                            }

                            let can_export = !self.tab.result.rows.is_empty();
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
                        let lines = self.tab.sql.split('\n').count();
                        let digits = lines.to_string().len().max(2);
                        let numbers = (1..=lines)
                            .map(|line| format!("{line:>digits$}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        ui.add(
                            egui::Label::new(theme::mono(numbers).color(theme::faint()))
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
                                    egui::TextEdit::multiline(&mut self.tab.sql)
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
                            .tab.selected
                            .as_ref()
                            .map(|object| object.name.clone())
                            .unwrap_or_else(|| "Consulta".to_string());
                        ui.label(theme::mono(title).color(theme::text()));

                        if self.tab.selected.is_some() {
                            let tabs = [(ResultsTab::Data, "Datos"), (ResultsTab::Schema, "Esquema")];
                            if theme::segmented(ui, &mut self.tab.results_tab, &tabs) {
                                self.tab.editing = None;
                            }
                        }
                        if self.tab.results_tab == ResultsTab::Schema && self.tab.selected.is_some() {
                            return;
                        }
                        let table_view = self.tab.selected.is_some()
                            && self
                                .tab.generated_sql
                                .as_deref()
                                .is_some_and(|generated| generated.trim() == self.tab.sql.trim());
                        if table_view {
                            let filter = ui.add(
                                egui::TextEdit::singleline(&mut self.tab.view.filter)
                                    .hint_text("Filtrar filas…")
                                    .desired_width(170.0),
                            );
                            if filter.changed() {
                                self.tab.view.page = 0;
                                self.load_view();
                            }
                        }

                        if !self.tab.result.columns.is_empty() {
                            let rows = self.tab.result.rows.len();
                            let summary = match self.tab.result.total {
                                Some(total) if rows > 0 => {
                                    let first = self.tab.view.page * db::ROW_LIMIT + 1;
                                    format!("filas {first}–{} de {total}", first + rows - 1)
                                }
                                Some(total) => format!("{total} filas"),
                                None => format!("{rows} filas"),
                            };
                            ui.label(
                                RichText::new(format!(
                                    "{summary} · {} ms",
                                    self.tab.result.elapsed.as_millis()
                                ))
                                .color(theme::muted()),
                            );
                            if self.tab.result.is_editable() {
                                theme::badge(
                                    ui,
                                    "Editable",
                                    theme::success(),
                                    theme::success().gamma_multiply(0.14),
                                );
                            } else {
                                theme::badge(ui, "Solo lectura", theme::muted(), theme::raised());
                            }
                        }

                        if self.tab.result.is_editable() {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add(egui::Button::new("+ Fila"))
                                        .on_hover_text("Insertar una fila nueva en la tabla.")
                                        .clicked()
                                    {
                                        self.open_insert_form();
                                    }
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(
                                                "Doble clic edita · Enter guarda · Esc cancela · clic derecho: más opciones",
                                            )
                                            .size(12.0)
                                            .color(theme::subtle()),
                                        )
                                        .truncate(),
                                    );
                                },
                            );
                        }
                    });
                });

            ui.add(egui::Separator::default().spacing(0.0));

            let schema_tab = self.tab.results_tab == ResultsTab::Schema && self.tab.selected.is_some();
            let paged = self
                .tab.result
                .total
                .filter(|total| !schema_tab && last_page(*total) > 0);
            Frame::NONE
                .inner_margin(Margin::symmetric(14, 8))
                .show(ui, |ui| {
                    if schema_tab {
                        self.render_schema(ui);
                        return;
                    }
                    // La rejilla deja sitio al pie (Live Watch y paginación) para que quede
                    // siempre visible al fondo de la tarjeta, debajo de las filas.
                    self.render_results(ui, FOOTER_HEIGHT);
                    ui.add_space((ui.available_height() - FOOTER_HEIGHT).max(0.0));
                    ui.add_space(6.0);
                    ui.add(egui::Separator::default().spacing(0.0));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        self.live_watch_selector(ui);
                        if let Some(total) = paged {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| self.pager(ui, last_page(total)),
                            );
                        }
                    });
                });
        });
    }

    fn render_schema(&mut self, ui: &mut egui::Ui) {
        let Some(name) = self.tab.selected.as_ref().map(|object| object.name.clone()) else {
            return;
        };
        if self.tab.tasks.is_running(TaskKind::Pull) {
            ui.weak("Extrayendo la base de datos…");
            return;
        }
        if self
            .tab
            .schema
            .as_ref()
            .is_none_or(|(loaded, _)| *loaded != name)
        {
            let schema = match &self.tab.db_path {
                Some(path) => db::open(path)
                    .and_then(|conn| db::table_schema(&conn, &name))
                    .map_err(|error| format!("{error:#}")),
                None => return,
            };
            self.tab.schema = Some((name, schema));
        }
        let Some((_, schema)) = &self.tab.schema else {
            return;
        };
        match schema {
            Ok(schema) => schema_view(ui, schema),
            Err(error) => {
                ui.label(
                    RichText::new(format!("No se pudo leer el esquema: {error}"))
                        .color(theme::danger()),
                );
            }
        }
    }

    fn live_watch_selector(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Live Watch").size(12.0).color(theme::muted()))
            .on_hover_text(if self.tab.target.is_some() {
                "Vuelve a extraer la base de datos del dispositivo y a ejecutar la consulta \
                 cada cierto tiempo. Se pausa mientras editas o hay cambios sin push."
            } else {
                "Vuelve a ejecutar la consulta cada cierto tiempo."
            });
        if theme::segmented(ui, &mut self.settings.live_watch_secs, &LIVE_WATCH_OPTIONS) {
            self.tab.last_refresh = Instant::now();
            self.persist();
        }
    }

    /// Controles de página, de derecha a izquierda: » › «Página n de m» ‹ «.
    fn pager(&mut self, ui: &mut egui::Ui, last_page: usize) {
        let page = self.tab.view.page;
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
                .color(theme::muted()),
        );
        nav(ui, "‹", page > 0, page.saturating_sub(1), "Página anterior");
        nav(ui, "«", page > 0, 0, "Primera página");
        if let Some(page) = target {
            self.go_to_page(page);
        }
    }

    /// Rejilla de resultados. `reserved` es la altura que debe quedar libre debajo (el pie de
    /// paginación).
    fn render_results(&mut self, ui: &mut egui::Ui, reserved: f32) {
        if self.tab.result.columns.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("Sin resultados")
                        .family(theme::semibold())
                        .color(theme::text_soft()),
                );
                ui.add_space(4.0);
                ui.weak("Ejecuta una consulta o selecciona una tabla.");
            });
            return;
        }

        let editable = self.tab.result.is_editable();
        let mut commit: Option<(usize, usize, String)> = None;
        let mut cancel = false;
        let mut action = None;
        // Solo las páginas de una tabla (las que traen total) se ordenan desde la cabecera.
        let sortable = self.tab.result.total.is_some();
        let mut sort_by: Option<String> = None;

        egui::ScrollArea::both()
            .id_salt("results_scroll")
            .auto_shrink([false, false])
            .max_height((ui.available_height() - reserved).max(80.0))
            .show(ui, |ui| {
                egui::Grid::new("results_grid")
                    .striped(true)
                    .min_col_width(72.0)
                    .min_row_height(26.0)
                    .spacing(egui::vec2(20.0, 4.0))
                    .show(ui, |ui| {
                        ui.label(theme::mono("#").color(theme::faint()));
                        for column in &self.tab.result.columns {
                            let arrow = match &self.tab.view.sort {
                                Some((sorted, false)) if sorted == column && sortable => " ▲",
                                Some((sorted, true)) if sorted == column && sortable => " ▼",
                                _ => "",
                            };
                            let color = if arrow.is_empty() {
                                theme::muted()
                            } else {
                                theme::accent()
                            };
                            let text = theme::strong(format!("{column}{arrow}"))
                                .size(12.5)
                                .color(color);
                            if sortable {
                                if ui
                                    .add(egui::Button::new(text).frame(false))
                                    .on_hover_text("Ordenar por esta columna")
                                    .clicked()
                                {
                                    sort_by = Some(column.clone());
                                }
                            } else {
                                ui.label(text);
                            }
                        }
                        ui.end_row();

                        for (row_index, row) in self.tab.result.rows.iter().enumerate() {
                            ui.label(
                                theme::mono((row_index + 1).to_string()).color(theme::faint()),
                            );

                            for (column_index, value) in row.iter().enumerate() {
                                let is_editing = self
                                    .tab
                                    .editing
                                    .as_ref()
                                    .map(|edit| {
                                        edit.row == row_index && edit.column == column_index
                                    })
                                    .unwrap_or(false);

                                if is_editing {
                                    let Some(mut edit) = self.tab.editing.take() else {
                                        continue;
                                    };
                                    let response = ui.add(
                                        egui::TextEdit::singleline(&mut edit.buffer)
                                            .desired_width(180.0)
                                            .font(egui::TextStyle::Monospace)
                                            .frame(
                                                Frame::NONE
                                                    .fill(theme::bg())
                                                    .stroke(Stroke::new(1.0, theme::accent()))
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
                                            self.tab.editing = None;
                                        } else {
                                            commit = Some((
                                                row_index,
                                                column_index,
                                                edit.buffer.clone(),
                                            ));
                                            self.tab.editing = Some(edit);
                                        }
                                    } else {
                                        self.tab.editing = Some(edit);
                                    }
                                } else {
                                    let preview = match value {
                                        Value::Text(text) => value_view::cell_preview(text),
                                        _ => None,
                                    };
                                    let text = match &preview {
                                        Some(preview) => {
                                            theme::mono(preview.as_str()).color(theme::text())
                                        }
                                        None => cell_text(value),
                                    };
                                    let response =
                                        ui.add(egui::Label::new(text).sense(egui::Sense::click()));
                                    let is_blob = matches!(value, Value::Blob(_));
                                    if response.double_clicked() {
                                        // Un BLOB o un texto largo no caben en el editor de
                                        // una línea: se abren en el visor.
                                        if editable && !is_blob && preview.is_none() {
                                            self.tab.editing = Some(CellEdit {
                                                row: row_index,
                                                column: column_index,
                                                buffer: if matches!(value, Value::Null) {
                                                    String::new()
                                                } else {
                                                    db::value_display(value)
                                                },
                                                focus_pending: true,
                                            });
                                        } else {
                                            action =
                                                Some(CellAction::View(row_index, column_index));
                                        }
                                    }
                                    response.context_menu(|ui| {
                                        let cell = (row_index, column_index);
                                        if ui.button("Ver valor").clicked() {
                                            action = Some(CellAction::View(cell.0, cell.1));
                                            ui.close();
                                        }
                                        if ui.button("Copiar valor").clicked() {
                                            action = Some(CellAction::Copy(cell.0, cell.1));
                                            ui.close();
                                        }
                                        if editable {
                                            ui.separator();
                                            if ui
                                                .add_enabled(
                                                    !matches!(value, Value::Null),
                                                    egui::Button::new("Poner a NULL"),
                                                )
                                                .clicked()
                                            {
                                                action = Some(CellAction::SetNull(cell.0, cell.1));
                                                ui.close();
                                            }
                                            let delete = egui::Button::new(
                                                RichText::new("Eliminar fila")
                                                    .color(theme::danger()),
                                            );
                                            if ui.add(delete).clicked() {
                                                action = Some(CellAction::Delete(cell.0));
                                                ui.close();
                                            }
                                        }
                                    });
                                }
                            }
                            ui.end_row();
                        }
                    });
            });

        if cancel {
            self.tab.editing = None;
            self.tab.status = "Edición cancelada.".to_string();
        }
        if let Some((row, column, value)) = commit {
            self.tab.editing = None;
            self.apply_edit(row, column, value);
        }
        if let Some(action) = action {
            self.apply_cell_action(&ui.ctx().clone(), action);
        }
        if let Some(column) = sort_by {
            self.toggle_sort(&column);
        }
    }
}

/// Tarjeta clicable de origen para la pantalla de bienvenida.
fn source_card(ui: &mut egui::Ui, mode: SourceMode, description: &str, active: bool) -> bool {
    let stroke = if active {
        Stroke::new(1.0, theme::accent())
    } else {
        Stroke::new(1.0, theme::border())
    };
    let frame = theme::card()
        .stroke(stroke)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(64.0);
            ui.label(theme::strong(mode.label()).size(14.0).color(theme::text()));
            ui.add_space(2.0);
            ui.label(RichText::new(description).size(12.5).color(theme::muted()));
        });
    let rect = frame.response.rect;
    let response = ui
        .interact(rect, ui.scope_id().with(mode.tag()), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.hovered() && !active {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(12),
            Stroke::new(1.0, theme::border_hover()),
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
                                .color(theme::warning()),
                        },
                        theme::mono(column.name.as_str()).color(theme::text()),
                        theme::mono(column.decl_type.as_str()).color(theme::keyword()),
                        if column.not_null {
                            RichText::new("NOT NULL")
                                .size(12.0)
                                .color(theme::text_soft())
                        } else {
                            RichText::new("sí").size(12.0).color(theme::subtle())
                        },
                        match &column.default {
                            Some(default) => theme::mono(default.as_str()).color(theme::string()),
                            None => RichText::new("—").color(theme::faint()),
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
                            theme::mono(index.name.as_str()).color(theme::text()),
                            theme::mono(index.columns.join(", ")).color(theme::ident()),
                            RichText::new(kind).size(12.0).color(theme::muted()),
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
                            theme::mono(key.from.join(", ")).color(theme::text()),
                            theme::mono(format!("{}({})", key.table, key.to.join(", ")))
                                .color(theme::ident()),
                            RichText::new(key.on_update.as_str())
                                .size(12.0)
                                .color(theme::muted()),
                            RichText::new(key.on_delete.as_str())
                                .size(12.0)
                                .color(theme::muted()),
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
    ui.label(RichText::new(title).color(theme::text_soft()));
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
                        .color(theme::muted()),
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
        (inserted, "+", theme::success()),
        (deleted, "−", theme::danger()),
        (updated, "~", theme::warning()),
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
            ui.label(theme::mono(table.name.as_str()).color(theme::text()));
            match table.change {
                TableChange::Added => {
                    let bg = theme::success().gamma_multiply(0.14);
                    theme::badge(ui, "nueva", theme::success(), bg);
                }
                TableChange::Removed => {
                    let bg = theme::danger().gamma_multiply(0.14);
                    theme::badge(ui, "eliminada", theme::danger(), bg);
                }
                TableChange::Modified => {}
            }
            if table.schema_changed {
                theme::badge(ui, "esquema cambiado", theme::warning(), theme::raised())
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
                diff_section(ui, "Insertadas", theme::success(), table.inserted.count);
                let rows = table.inserted.rows.iter().map(|row| (row.as_slice(), None));
                diff_grid(ui, (&table.name, "ins"), &table.columns, rows);
            }
            if table.deleted.count > 0 {
                diff_section(ui, "Eliminadas", theme::danger(), table.deleted.count);
                let rows = table.deleted.rows.iter().map(|row| (row.as_slice(), None));
                diff_grid(ui, (&table.name, "del"), &table.columns, rows);
            }
            if table.updated.count > 0 {
                diff_section(ui, "Modificadas", theme::warning(), table.updated.count);
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
        ui.label(RichText::new(format!("{title} · {count}")).color(theme::text_soft()));
        if count > diff::MAX_ROWS as u64 {
            ui.label(
                RichText::new(format!("se muestran las primeras {}", diff::MAX_ROWS))
                    .size(12.0)
                    .color(theme::subtle()),
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
                                .color(theme::muted()),
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
                                        .color(theme::warning()),
                                    );
                                }
                                Some(_) => {
                                    ui.label(
                                        theme::mono(db::value_display(value)).color(theme::faint()),
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
fn other_mobile_mode(mode: SourceMode) -> Option<SourceMode> {
    match mode {
        SourceMode::Ios => Some(SourceMode::Android),
        SourceMode::Android => Some(SourceMode::Ios),
        SourceMode::Local => None,
    }
}

fn last_page(total: u64) -> usize {
    (total as usize).saturating_sub(1) / db::ROW_LIMIT
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Último tramo de una ruta relativa del dispositivo: `databases/offline.db` → `offline.db`.
fn last_segment(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn cell_text(value: &Value) -> RichText {
    match value {
        Value::Null => RichText::new(" NULL ")
            .monospace()
            .italics()
            .color(theme::subtle())
            .background_color(theme::hover()),
        Value::Integer(_) | Value::Real(_) => {
            theme::mono(db::value_display(value)).color(theme::number())
        }
        Value::Blob(_) => theme::mono(db::value_display(value)).color(theme::muted()),
        Value::Text(_) => theme::mono(db::value_display(value)).color(theme::text()),
    }
}

impl eframe::App for DbBridgeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        theme::sync(&ctx);
        self.poll_tasks();
        self.handle_dropped_files(&ctx);
        self.tick_live_watch(&ctx);

        self.update_banner(ui);
        self.tab_bar(ui);
        self.top_panel(ui);
        self.status_panel(ui);
        self.side_panel(ui);
        self.central_panel(ui);
        self.dialogs(&ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_segment_keeps_the_file_name() {
        assert_eq!(last_segment("databases/offline.db"), "offline.db");
        assert_eq!(
            last_segment("Library/Application Support/default.store"),
            "default.store"
        );
        assert_eq!(last_segment("offline.db"), "offline.db");
    }

    #[test]
    fn sibling_tab_copies_the_selection_but_not_the_connection() {
        let ctx = egui::Context::default();
        let mut tab = Tab::new(&ctx, 0, SourceMode::Ios);
        tab.device = "A1B2".to_string();
        tab.app_id = "com.example.app".to_string();
        tab.db_name = "Documents/offline.db".to_string();
        tab.detected_dbs = vec![
            "Documents/offline.db".to_string(),
            "Documents/cache.db".to_string(),
        ];
        tab.db_path = Some(PathBuf::from("/tmp/offline.db"));
        tab.target = Some(MobileTarget {
            mode: SourceMode::Ios,
            device: "A1B2".to_string(),
            app_id: "com.example.app".to_string(),
            db_name: "Documents/offline.db".to_string(),
        });
        tab.dirty = true;
        assert_eq!(tab.title(), "iOS · offline.db");

        let sibling = tab.sibling(&ctx, 1);
        assert_eq!(sibling.id, 1);
        assert_eq!(sibling.mode, SourceMode::Ios);
        assert_eq!(sibling.device, "A1B2");
        assert_eq!(sibling.app_id, "com.example.app");
        assert_eq!(sibling.detected_dbs, tab.detected_dbs);
        assert!(sibling.db_path.is_none() && sibling.target.is_none());
        assert!(!sibling.dirty && !sibling.has_pending_work());
        assert_eq!(sibling.title(), "iOS · sin conexión");
    }
}
