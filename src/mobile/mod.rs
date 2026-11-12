pub mod adb;
pub mod ios;

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

use crate::model::SourceMode;

/// Un simulador encendido o un dispositivo/emulador conectado por adb.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// UDID del simulador o serial de adb.
    pub id: String,
    pub name: String,
}

/// Valida el UDID o serial elegido. Vacío significa «el único encendido/conectado».
pub fn validate_device(value: &str) -> Result<()> {
    let valid = value.is_empty()
        || (value.len() <= 128
            && !value.starts_with('-')
            && value.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-' | ':')
            }));
    if !valid {
        bail!("identificador de dispositivo inválido: {value}");
    }
    Ok(())
}

pub fn validate_component(value: &str, label: &str) -> Result<()> {
    if !is_safe_component(value, false) {
        bail!("{label} inválido: usa letras, números y los caracteres . _ - /");
    }
    Ok(())
}

/// Valida el nombre o la ruta relativa de la BD. En iOS no hay shell de por medio
/// (`Library/Application Support` lleva un espacio); en Android sí, así que allí no se admiten.
pub fn validate_db_name(mode: SourceMode, value: &str) -> Result<()> {
    if !is_safe_component(value, mode == SourceMode::Ios) {
        bail!(
            "nombre de la base de datos inválido: usa una ruta relativa con letras, números y los caracteres . _ - /"
        );
    }
    Ok(())
}

fn is_safe_component(value: &str, allow_space: bool) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 256
        && !trimmed.starts_with('/')
        && !trimmed.contains("..")
        && trimmed.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '.' | '_' | '-' | '/')
                || (allow_space && character == ' ')
        })
}

/// Archivos auxiliares de SQLite que nunca son la base de datos principal.
fn is_sidecar(path: &str) -> bool {
    ["-wal", "-shm", "-journal"]
        .iter()
        .any(|suffix| path.ends_with(suffix))
}

pub fn staging_dir(mode: SourceMode, app_id: &str) -> PathBuf {
    std::env::temp_dir()
        .join("db-bridge")
        .join(mode.tag())
        .join(sanitize(app_id))
}

fn sanitize(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | '@') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

pub fn pull(mode: SourceMode, device: &str, app_id: &str, db_name: &str) -> Result<PathBuf> {
    validate_device(device)?;
    validate_component(app_id, "identificador de la app")?;
    validate_db_name(mode, db_name)?;
    let app_id = app_id.trim();
    let db_name = db_name.trim();
    let staging = staging_dir(mode, app_id);

    match mode {
        SourceMode::Local => bail!("el modo local no necesita extracción"),
        SourceMode::Ios => ios::pull(device, app_id, db_name, &staging),
        SourceMode::Android => adb::pull(device, app_id, db_name, &staging),
    }
}

pub fn push(
    mode: SourceMode,
    device: &str,
    app_id: &str,
    db_name: &str,
    local: &Path,
) -> Result<()> {
    validate_device(device)?;
    validate_component(app_id, "identificador de la app")?;
    validate_db_name(mode, db_name)?;
    let app_id = app_id.trim();
    let db_name = db_name.trim();

    match mode {
        SourceMode::Local => bail!("el modo local no necesita push"),
        SourceMode::Ios => ios::push(device, app_id, db_name, local),
        SourceMode::Android => adb::push(device, app_id, db_name, local),
    }
}

/// Copia la BD local a `push/` junto a ella, con el mismo nombre de archivo, para enviarla desde
/// otro hilo mientras la copia de trabajo sigue recibiendo ediciones. Llamar tras un checkpoint:
/// solo se copia el archivo principal.
pub fn push_snapshot(local: &Path) -> Result<PathBuf> {
    let (Some(parent), Some(file_name)) = (local.parent(), local.file_name()) else {
        bail!("ruta de base de datos no válida: {}", local.display());
    };
    let directory = parent.join("push");
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("no se pudo crear {}", directory.display()))?;
    let snapshot = directory.join(file_name);
    std::fs::copy(local, &snapshot)
        .with_context(|| format!("no se pudo copiar {}", local.display()))?;
    Ok(snapshot)
}

pub fn detect_devices(mode: SourceMode) -> Result<Vec<Device>> {
    match mode {
        SourceMode::Local => Ok(Vec::new()),
        SourceMode::Ios => ios::list_devices(),
        SourceMode::Android => adb::list_devices(),
    }
}

pub fn detect_apps(mode: SourceMode, device: &str) -> Result<Vec<String>> {
    validate_device(device)?;
    match mode {
        SourceMode::Local => Ok(Vec::new()),
        SourceMode::Ios => ios::list_apps(device),
        SourceMode::Android => adb::list_packages(device),
    }
}

/// Rutas relativas al contenedor de la app de los archivos que parecen bases de datos SQLite.
pub fn detect_databases(mode: SourceMode, device: &str, app_id: &str) -> Result<Vec<String>> {
    validate_device(device)?;
    validate_component(app_id, "identificador de la app")?;
    let app_id = app_id.trim();
    let mut databases = match mode {
        SourceMode::Local => Vec::new(),
        SourceMode::Ios => ios::list_databases(device, app_id)?,
        SourceMode::Android => adb::list_databases(device, app_id)?,
    };
    databases.retain(|path| !is_sidecar(path) && validate_db_name(mode, path).is_ok());
    // Primero las menos anidadas: suelen ser las de la propia app y no las de una librería.
    databases.sort_by(|a, b| {
        (a.matches('/').count(), a.as_str()).cmp(&(b.matches('/').count(), b.as_str()))
    });
    databases.dedup();
    Ok(databases)
}

pub fn command_error(program: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        format!("{program} terminó con {}", output.status)
    } else {
        format!("{program}: {stderr}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_realistic_identifiers() {
        validate_component("com.example.my_app", "package").unwrap();
        validate_component("offline.db", "archivo").unwrap();
        validate_component("databases/offline.db", "archivo").unwrap();
    }

    #[test]
    fn rejects_shell_metacharacters_and_traversal() {
        assert!(validate_component("com.example; rm -rf /", "package").is_err());
        assert!(validate_component("../../etc/passwd", "archivo").is_err());
        assert!(validate_component("", "package").is_err());
        assert!(validate_component("app name", "package").is_err());
        assert!(validate_component("/etc/passwd", "archivo").is_err());
    }

    #[test]
    fn device_ids_accept_serials_and_udids_only() {
        validate_device("").unwrap();
        validate_device("emulator-5554").unwrap();
        validate_device("192.168.1.20:5555").unwrap();
        validate_device("A1B2C3D4-0000-4E5F-8A9B-112233445566").unwrap();
        assert!(validate_device("-s").is_err());
        assert!(validate_device("emulator-5554; reboot").is_err());
        assert!(validate_device("a b").is_err());
    }

    #[test]
    fn db_name_allows_spaces_only_on_ios() {
        let path = "Library/Application Support/default.store";
        validate_db_name(SourceMode::Ios, path).unwrap();
        assert!(validate_db_name(SourceMode::Android, path).is_err());
        assert!(validate_db_name(SourceMode::Ios, "/Library/x.db").is_err());
        assert!(validate_db_name(SourceMode::Ios, "Documents/../../x.db").is_err());
    }

    #[test]
    fn detects_sqlite_sidecar_files() {
        assert!(is_sidecar("databases/app.db-wal"));
        assert!(is_sidecar("databases/app.db-journal"));
        assert!(!is_sidecar("databases/app.db"));
    }

    #[test]
    fn staging_directory_is_per_mode_and_app() {
        let ios = staging_dir(SourceMode::Ios, "com.example.app");
        let android = staging_dir(SourceMode::Android, "com.example.app");
        assert!(ios.ends_with("ios/com.example.app"));
        assert!(android.ends_with("android/com.example.app"));
        assert_ne!(ios, android);
    }

    #[test]
    fn push_snapshot_keeps_the_file_name() {
        let directory = tempfile::tempdir().unwrap();
        let local = directory.path().join("offline.db");
        std::fs::write(&local, b"contenido").unwrap();

        let snapshot = push_snapshot(&local).unwrap();
        assert_eq!(snapshot, directory.path().join("push").join("offline.db"));

        std::fs::write(&local, b"editado despues").unwrap();
        assert_eq!(std::fs::read(&snapshot).unwrap(), b"contenido");
    }
}
