use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::update_check::Release;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum SourceMode {
    #[default]
    Local,
    Ios,
    Android,
}

impl SourceMode {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Ios => "ios",
            Self::Android => "android",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "Archivo local",
            Self::Ios => "Simulador iOS",
            Self::Android => "Emulador Android",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FavoriteQuery {
    pub name: String,
    pub sql: String,
}

/// Una conexión reciente: un archivo local o una app y su archivo de BD en un dispositivo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecentSource {
    pub mode: SourceMode,
    /// UDID o serial del dispositivo; vacío en los archivos locales y en recientes antiguos.
    #[serde(default)]
    pub device: String,
    pub app_id: String,
    pub db_name: String,
    pub path: Option<PathBuf>,
}

impl RecentSource {
    pub fn label(&self) -> String {
        match &self.path {
            Some(path) => path.display().to_string(),
            None => format!("{}/{}", self.app_id, self.db_name),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub mode: SourceMode,
    /// UDID o serial del dispositivo elegido; vacío para usar el único encendido/conectado.
    pub device: String,
    pub app_id: String,
    pub db_name: String,
    pub favorites: Vec<FavoriteQuery>,
    pub recents: Vec<RecentSource>,
    /// Envía al dispositivo cada cambio en cuanto se aplica en la copia local.
    pub auto_push: bool,
    /// Últimas consultas escritas en el editor, de la más reciente a la más antigua.
    pub history: Vec<String>,
    /// Intervalo de Live Watch en segundos; 0 lo apaga.
    pub live_watch_secs: u64,
    /// Consulta en GitHub, como mucho una vez al día, si hay una versión nueva.
    pub check_updates: bool,
    /// Momento (segundos Unix) de la última consulta de versiones.
    pub last_update_check: u64,
    /// Última Release estable conocida; se guarda para seguir avisando entre consultas.
    pub latest_release: Option<Release>,
    /// Versión que el usuario decidió omitir.
    pub skipped_version: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: SourceMode::default(),
            device: String::new(),
            app_id: String::new(),
            db_name: String::new(),
            favorites: Vec::new(),
            recents: Vec::new(),
            auto_push: true,
            history: Vec::new(),
            live_watch_secs: 2,
            check_updates: true,
            last_update_check: 0,
            latest_release: None,
            skipped_version: String::new(),
        }
    }
}
