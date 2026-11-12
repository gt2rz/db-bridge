use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
        }
    }
}
