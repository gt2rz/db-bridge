use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use super::{Device, command_error};

const ADB: &str = if cfg!(windows) { "adb.exe" } else { "adb" };

/// Ruta de `adb`. Una app abierta desde Finder o el menú de inicio no hereda el `PATH` de la
/// terminal, así que además del `PATH` se prueban `ANDROID_HOME`, `ANDROID_SDK_ROOT` y las
/// carpetas por defecto del SDK y de Homebrew.
fn adb_program() -> &'static Path {
    static PROGRAM: OnceLock<PathBuf> = OnceLock::new();
    PROGRAM.get_or_init(|| {
        let in_path = std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
            .unwrap_or_default();
        let sdk_roots = ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
            .into_iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
            .chain(dirs::home_dir().map(|home| {
                if cfg!(target_os = "macos") {
                    home.join("Library/Android/sdk")
                } else {
                    home.join("Android/Sdk")
                }
            }))
            .chain(dirs::data_local_dir().map(|data| data.join("Android/Sdk")))
            .map(|root| root.join("platform-tools"));
        let fixed = ["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from);

        in_path
            .into_iter()
            .chain(sdk_roots)
            .chain(fixed)
            .map(|directory| directory.join(ADB))
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| PathBuf::from(ADB))
    })
}

/// Ejecuta `adb`, contra `device` si se indica (`-s <serial>`) o contra el único conectado.
fn adb(device: &str, args: &[&str]) -> Result<std::process::Output> {
    let mut command = Command::new(adb_program());
    if !device.is_empty() {
        command.args(["-s", device]);
    }
    command.args(args).output().context(
        "no se pudo ejecutar adb; instala Android SDK Platform Tools o añade su carpeta al PATH",
    )
}

fn adb_shell(device: &str, args: &[&str]) -> Result<std::process::Output> {
    let mut full = Vec::with_capacity(args.len() + 1);
    full.push("shell");
    full.extend_from_slice(args);
    adb(device, &full)
}

pub fn list_devices() -> Result<Vec<Device>> {
    let output = adb("", &["devices", "-l"]).context("no se pudo listar los dispositivos")?;
    if !output.status.success() {
        bail!(
            "no se pudo listar los dispositivos: {}",
            command_error("adb", &output)
        );
    }
    Ok(parse_devices(&String::from_utf8_lossy(&output.stdout)))
}

/// Interpreta `adb devices -l`. Solo cuenta los dispositivos listos (`device`): los
/// `unauthorized` u `offline` no admiten órdenes.
fn parse_devices(stdout: &str) -> Vec<Device> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let serial = fields.next()?;
            if fields.next()? != "device" {
                return None;
            }
            let model = fields
                .find_map(|field| field.strip_prefix("model:"))
                .map(|model| model.replace('_', " "));
            let name = match model {
                Some(model) => format!("{model} · {serial}"),
                None => serial.to_string(),
            };
            Some(Device {
                id: serial.to_string(),
                name,
            })
        })
        .collect()
}

pub fn list_packages(device: &str) -> Result<Vec<String>> {
    let output = adb(device, &["shell", "pm", "list", "packages", "-3"])
        .context("no se pudo consultar los paquetes instalados")?;
    if !output.status.success() {
        bail!(
            "no se pudo listar los paquetes: {}",
            command_error("adb", &output)
        );
    }

    let mut packages = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().strip_prefix("package:"))
        .map(|package| package.trim().to_string())
        .filter(|package| !package.is_empty())
        .collect::<Vec<_>>();
    packages.sort();
    packages.dedup();
    Ok(packages)
}

/// Busca candidatos con un solo `find`: todo lo que haya en `databases/` (Room suele crear
/// archivos sin extensión) y los archivos con extensión de SQLite en el resto del contenedor.
pub fn list_databases(device: &str, package: &str) -> Result<Vec<String>> {
    // `adb shell` pasa la orden a un shell remoto: el patrón va como una sola cadena y entre
    // comillas para que el shell no expanda los comodines. `package` ya está validado.
    let command = format!(
        "run-as {package} find . -maxdepth 6 -type f \\( -path './databases/*' \
         -o -name '*.db' -o -name '*.db3' -o -name '*.sqlite' -o -name '*.sqlite3' \\)"
    );
    let output = adb_shell(device, &[&command])
        .context("no se pudo buscar bases de datos en el dispositivo")?;
    if !output.status.success() {
        bail!(
            "no se pudo listar el contenedor de {package}. ¿La app está compilada en modo debug? ({})",
            command_error("adb", &output)
        );
    }
    Ok(parse_find_output(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_find_output(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.trim_start_matches("./").to_string())
        .collect()
}

fn find_db_relpath(device: &str, package: &str, db_name: &str) -> Result<String> {
    // Una ruta relativa (la que da la detección) se usa tal cual si existe.
    if db_name.contains('/') {
        let output = adb_shell(device, &["run-as", package, "ls", db_name])
            .context("no se pudo comprobar la ruta en el dispositivo")?;
        if output.status.success() {
            return Ok(db_name.to_string());
        }
        bail!("no se encontró «{db_name}» en el contenedor de {package}");
    }

    if let Ok(output) = adb_shell(device, &["run-as", package, "ls", "databases"])
        && output.status.success()
    {
        let listing = String::from_utf8_lossy(&output.stdout);
        for line in listing.lines() {
            if line.trim() == db_name {
                return Ok(format!("databases/{db_name}"));
            }
        }
    }

    let output = adb_shell(
        device,
        &[
            "run-as",
            package,
            "find",
            ".",
            "-maxdepth",
            "6",
            "-type",
            "f",
            "-name",
            db_name,
        ],
    )
    .context("no se pudo buscar la base de datos en el dispositivo")?;

    if output.status.success()
        && let Some(path) = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
    {
        return Ok(path.trim_start_matches("./").to_string());
    }

    bail!(
        "no se encontró «{db_name}» en el contenedor de {package}. \
         Comprueba que el APK sea debuggable y que el nombre sea correcto"
    )
}

pub fn pull(device: &str, package: &str, db_name: &str, staging: &Path) -> Result<PathBuf> {
    let remote = find_db_relpath(device, package, db_name)?;
    std::fs::create_dir_all(staging)
        .with_context(|| format!("no se pudo crear {}", staging.display()))?;

    let file_name = Path::new(db_name)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("database.db");
    let destination = staging.join(file_name);

    for suffix in ["", "-wal", "-shm"] {
        let remote_path = format!("{remote}{suffix}");
        let local_path = PathBuf::from(format!("{}{}", destination.display(), suffix));

        let output = adb(
            device,
            &["exec-out", "run-as", package, "cat", &remote_path],
        )
        .context("no se pudo leer la base de datos con adb exec-out")?;

        if output.status.success() && !output.stdout.is_empty() {
            std::fs::write(&local_path, &output.stdout)
                .with_context(|| format!("no se pudo escribir {}", local_path.display()))?;
        } else if suffix.is_empty() {
            bail!(
                "no se pudo extraer {remote_path}. ¿La app está compilada en modo debug? ({})",
                command_error("adb", &output)
            );
        } else {
            let _ = std::fs::remove_file(&local_path);
        }
    }

    Ok(destination)
}

pub fn push(device: &str, package: &str, db_name: &str, local: &Path) -> Result<()> {
    let remote = find_db_relpath(device, package, db_name)?;

    let _ = adb_shell(device, &["am", "force-stop", package]);

    let file_name = local
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("database.db");
    let remote_tmp = format!("/data/local/tmp/db-bridge-{file_name}");

    let output = adb(device, &["push", &local.display().to_string(), &remote_tmp])
        .context("no se pudo copiar la base de datos al dispositivo")?;
    if !output.status.success() {
        bail!("adb push falló: {}", command_error("adb", &output));
    }

    for suffix in ["-wal", "-shm"] {
        let _ = adb_shell(
            device,
            &["run-as", package, "rm", "-f", &format!("{remote}{suffix}")],
        );
    }

    let output = adb_shell(device, &["run-as", package, "cp", &remote_tmp, &remote])
        .context("no se pudo escribir la base de datos en el contenedor de la app")?;

    let _ = adb_shell(device, &["rm", "-f", &remote_tmp]);

    if !output.status.success() {
        bail!(
            "no se pudo escribir {remote}: {}",
            command_error("adb", &output)
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ready_devices_with_their_model() {
        let stdout = "List of devices attached\n\
            emulator-5554          device product:sdk_gphone64_arm64 model:sdk_gphone64_arm64 device:emu64a transport_id:1\n\
            R58N123ABC             unauthorized usb:1-1 transport_id:2\n\
            192.168.1.20:5555      device product:panther model:Pixel_7 device:panther transport_id:3\n\
            \n";
        assert_eq!(
            parse_devices(stdout),
            vec![
                Device {
                    id: "emulator-5554".to_string(),
                    name: "sdk gphone64 arm64 · emulator-5554".to_string(),
                },
                Device {
                    id: "192.168.1.20:5555".to_string(),
                    name: "Pixel 7 · 192.168.1.20:5555".to_string(),
                },
            ]
        );
    }

    #[test]
    fn parses_find_output_into_relative_paths() {
        let stdout =
            "./databases/app_database\r\n./databases/app_database-wal\n\n./files/cache.sqlite\n";
        assert_eq!(
            parse_find_output(stdout),
            vec![
                "databases/app_database",
                "databases/app_database-wal",
                "files/cache.sqlite"
            ]
        );
    }
}
