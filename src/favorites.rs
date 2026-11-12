use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::model::Settings;

fn settings_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("db-bridge")
        .join("settings.json")
}

pub fn load() -> Settings {
    let Ok(data) = std::fs::read_to_string(settings_path()) else {
        return Settings::default();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

pub fn save(settings: &Settings) -> Result<()> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("no se pudo crear {}", parent.display()))?;
    }

    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(settings)?)
        .with_context(|| format!("no se pudo escribir {}", temporary.display()))?;
    std::fs::rename(&temporary, &path)
        .with_context(|| format!("no se pudo guardar {}", path.display()))?;
    Ok(())
}
