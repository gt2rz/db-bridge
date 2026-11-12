use anyhow::{Context, Result, bail};
use serde_json::Value as Json;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{Device, command_error};

const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
const MAX_DEPTH: usize = 6;

const SEARCH_DIRS: [&str; 5] = [
    "Documents",
    "Library/Application Support",
    "Library",
    "Library/Caches",
    "tmp",
];

/// UDID del simulador elegido, o `booted` (el que simctl considere encendido) si no hay ninguno.
fn simulator(device: &str) -> &str {
    if device.is_empty() { "booted" } else { device }
}

pub fn list_devices() -> Result<Vec<Device>> {
    let output = Command::new("xcrun")
        .args(["simctl", "list", "devices", "booted", "--json"])
        .output()
        .context("no se pudo ejecutar xcrun; ¿está Xcode instalado?")?;
    if !output.status.success() {
        bail!(
            "no se pudo listar los simuladores: {}",
            command_error("xcrun", &output)
        );
    }
    let json: Json = serde_json::from_slice(&output.stdout)
        .context("la lista de simuladores no es JSON válido")?;
    Ok(parse_devices(&json))
}

/// Interpreta `simctl list devices --json`: `{"devices": {"<runtime>": [{udid, name, state}]}}`.
fn parse_devices(json: &Json) -> Vec<Device> {
    let mut devices = Vec::new();
    let Some(runtimes) = json.get("devices").and_then(Json::as_object) else {
        return devices;
    };
    for (runtime, entries) in runtimes {
        let version = runtime_label(runtime);
        for entry in entries.as_array().into_iter().flatten() {
            let state = entry.get("state").and_then(Json::as_str);
            let (Some(udid), Some(name)) = (
                entry.get("udid").and_then(Json::as_str),
                entry.get("name").and_then(Json::as_str),
            ) else {
                continue;
            };
            if state != Some("Booted") {
                continue;
            }
            devices.push(Device {
                id: udid.to_string(),
                name: format!("{name} · {version}"),
            });
        }
    }
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    devices
}

/// `com.apple.CoreSimulator.SimRuntime.iOS-18-2` → `iOS 18.2`. Los runtimes con sufijo
/// (`iOS-27-2-iPhone19-4`) se quedan en la plataforma y la versión.
fn runtime_label(runtime: &str) -> String {
    let name = runtime.rsplit('.').next().unwrap_or(runtime);
    let mut parts = name.split('-');
    let platform = parts.next().unwrap_or(name);
    let version = parts
        .take_while(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
        .collect::<Vec<_>>()
        .join(".");
    if version.is_empty() {
        platform.to_string()
    } else {
        format!("{platform} {version}")
    }
}

fn container(device: &str, bundle_id: &str) -> Result<PathBuf> {
    let output = Command::new("xcrun")
        .args([
            "simctl",
            "get_app_container",
            simulator(device),
            bundle_id,
            "data",
        ])
        .output()
        .context("no se pudo ejecutar xcrun; ¿está Xcode instalado?")?;

    if !output.status.success() {
        bail!(
            "no se encontró {bundle_id} en el simulador encendido ({})",
            command_error("xcrun", &output)
        );
    }

    let path = String::from_utf8(output.stdout)
        .context("la ruta del contenedor no es UTF-8")?
        .trim()
        .to_string();
    if path.is_empty() {
        bail!("el simulador devolvió un contenedor vacío");
    }
    Ok(PathBuf::from(path))
}

pub fn find_db(device: &str, bundle_id: &str, db_name: &str) -> Result<PathBuf> {
    let root = container(device, bundle_id)?;

    // Una ruta relativa (la que da la detección) se usa tal cual.
    if db_name.contains('/') {
        let candidate = root.join(db_name);
        if candidate.is_file() {
            return Ok(candidate);
        }
        bail!("no se encontró «{db_name}» dentro del contenedor de {bundle_id}");
    }

    for relative in SEARCH_DIRS {
        let candidate = root.join(relative).join(db_name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    if let Some(found) = walk(&root, db_name, MAX_DEPTH) {
        return Ok(found);
    }

    bail!(
        "no se encontró «{db_name}» dentro del contenedor de {bundle_id}. \
         Revisa el nombre del archivo o la carpeta usada por tu framework"
    )
}

fn walk(directory: &Path, file_name: &str, depth: usize) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let entries = std::fs::read_dir(directory).ok()?;
    let mut subdirectories = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        if path.is_file() && name.as_ref() == file_name {
            return Some(path);
        }
        if path.is_dir() {
            subdirectories.push(path);
        }
    }

    for subdirectory in subdirectories {
        if let Some(found) = walk(&subdirectory, file_name, depth - 1) {
            return Some(found);
        }
    }
    None
}

pub fn list_databases(device: &str, bundle_id: &str) -> Result<Vec<String>> {
    let root = container(device, bundle_id)?;
    let mut found = Vec::new();
    collect_databases(&root, &root, MAX_DEPTH, &mut found);
    Ok(found)
}

/// Recorre el contenedor y guarda la ruta relativa de cada archivo con cabecera SQLite.
fn collect_databases(root: &Path, directory: &Path, depth: usize, found: &mut Vec<String>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect_databases(root, &path, depth - 1, found);
        } else if has_sqlite_header(&path)
            && let Ok(relative) = path.strip_prefix(root)
        {
            let relative = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            found.push(relative);
        }
    }
}

fn has_sqlite_header(path: &Path) -> bool {
    let mut header = [0u8; 16];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .is_ok()
        && &header == SQLITE_HEADER
}

pub fn pull(device: &str, bundle_id: &str, db_name: &str, staging: &Path) -> Result<PathBuf> {
    let source = find_db(device, bundle_id, db_name)?;
    std::fs::create_dir_all(staging)
        .with_context(|| format!("no se pudo crear {}", staging.display()))?;

    let file_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("database.db");
    let destination = staging.join(file_name);

    let _ = std::fs::remove_file(&destination);
    std::fs::copy(&source, &destination)
        .with_context(|| format!("no se pudo copiar {}", source.display()))?;

    for suffix in ["-wal", "-shm"] {
        let wal_source = PathBuf::from(format!("{}{}", source.display(), suffix));
        let wal_destination = PathBuf::from(format!("{}{}", destination.display(), suffix));
        if wal_source.is_file() {
            std::fs::copy(&wal_source, &wal_destination)?;
        } else {
            let _ = std::fs::remove_file(&wal_destination);
        }
    }

    Ok(destination)
}

pub fn push(device: &str, bundle_id: &str, db_name: &str, local: &Path) -> Result<()> {
    let target = find_db(device, bundle_id, db_name)?;

    let _ = Command::new("xcrun")
        .args(["simctl", "terminate", simulator(device), bundle_id])
        .output();

    std::fs::copy(local, &target)
        .with_context(|| format!("no se pudo escribir {}", target.display()))?;

    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(PathBuf::from(format!("{}{}", target.display(), suffix)));
    }

    Ok(())
}

pub fn list_apps(device: &str) -> Result<Vec<String>> {
    let output = Command::new("xcrun")
        .args(["simctl", "listapps", simulator(device)])
        .output()
        .context("no se pudo ejecutar xcrun simctl listapps")?;

    if !output.status.success() {
        bail!(
            "no hay un simulador de iOS encendido ({})",
            command_error("xcrun", &output)
        );
    }

    let mut child = Command::new("plutil")
        .args(["-convert", "json", "-o", "-", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("no se pudo ejecutar plutil")?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&output.stdout)
            .context("no se pudo enviar la lista de apps a plutil")?;
    }

    let converted = child
        .wait_with_output()
        .context("plutil no terminó correctamente")?;
    if !converted.status.success() {
        bail!(
            "no se pudo convertir la lista de apps: {}",
            command_error("plutil", &converted)
        );
    }

    let json: Json =
        serde_json::from_slice(&converted.stdout).context("la lista de apps no es JSON válido")?;

    let mut apps = Vec::new();
    if let Some(map) = json.as_object() {
        for (key, info) in map {
            let bundle = info
                .get("CFBundleIdentifier")
                .and_then(Json::as_str)
                .unwrap_or(key);
            let app_type = info
                .get("ApplicationType")
                .and_then(Json::as_str)
                .unwrap_or("");
            if app_type == "System" || bundle.starts_with("com.apple.") {
                continue;
            }
            apps.push(bundle.to_string());
        }
    }

    apps.sort();
    apps.dedup();
    Ok(apps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_booted_simulators_with_their_runtime() {
        let json: Json = serde_json::from_str(
            r#"{"devices": {
                "com.apple.CoreSimulator.SimRuntime.iOS-18-2": [
                    {"udid": "A1B2-C3", "name": "iPhone 16", "state": "Booted"},
                    {"udid": "D4E5-F6", "name": "iPad Air", "state": "Shutdown"}
                ],
                "com.apple.CoreSimulator.SimRuntime.watchOS-11-0": []
            }}"#,
        )
        .unwrap();
        assert_eq!(
            parse_devices(&json),
            vec![Device {
                id: "A1B2-C3".to_string(),
                name: "iPhone 16 · iOS 18.2".to_string(),
            }]
        );
    }

    #[test]
    fn labels_runtimes_with_platform_and_version() {
        assert_eq!(
            runtime_label("com.apple.CoreSimulator.SimRuntime.iOS-18-2"),
            "iOS 18.2"
        );
        assert_eq!(
            runtime_label("com.apple.CoreSimulator.SimRuntime.iOS-27-2-iPhone19-4"),
            "iOS 27.2"
        );
        assert_eq!(
            runtime_label("com.apple.CoreSimulator.SimRuntime.xrOS-2-0"),
            "xrOS 2.0"
        );
    }

    #[test]
    fn finds_database_in_nested_framework_folder() {
        let root = tempfile::tempdir().unwrap();
        let nested = root
            .path()
            .join("Library")
            .join("Application Support")
            .join("com.example.app");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("offline.db"), b"sqlite").unwrap();

        let found = walk(root.path(), "offline.db", 6).unwrap();
        assert_eq!(found, nested.join("offline.db"));
    }

    #[test]
    fn ignores_hidden_directories() {
        let root = tempfile::tempdir().unwrap();
        let hidden = root.path().join(".secret");
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::write(hidden.join("offline.db"), b"sqlite").unwrap();

        assert!(walk(root.path(), "offline.db", 6).is_none());
    }

    #[test]
    fn lists_only_files_with_sqlite_header() {
        let root = tempfile::tempdir().unwrap();
        let support = root.path().join("Library").join("Application Support");
        std::fs::create_dir_all(&support).unwrap();
        let mut sqlite = SQLITE_HEADER.to_vec();
        sqlite.extend_from_slice(&[0; 84]);
        std::fs::write(support.join("default.store"), &sqlite).unwrap();
        std::fs::write(root.path().join("notes.txt"), b"not a database").unwrap();
        std::fs::write(root.path().join("tiny"), b"SQL").unwrap();

        let mut found = Vec::new();
        collect_databases(root.path(), root.path(), MAX_DEPTH, &mut found);
        assert_eq!(found, vec!["Library/Application Support/default.store"]);
    }

    #[test]
    fn respects_depth_limit() {
        let root = tempfile::tempdir().unwrap();
        let deep = root.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("offline.db"), b"sqlite").unwrap();

        assert!(walk(root.path(), "offline.db", 2).is_none());
        assert!(walk(root.path(), "offline.db", 4).is_some());
    }
}
