//! Instalación de una versión nueva desde la propia app: descarga el binario de la Release para
//! este sistema, comprueba su SHA-256 y sustituye la instalación actual. El reinicio va aparte y
//! lo pide el usuario, para no cortar un push ni dejar cambios sin enviar.
//!
//! Todo el código se compila en los tres sistemas (solo cambia qué rama se ejecuta) para que
//! clippy y los tests lo revisen en cualquier máquina.

use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::mobile::command_error;
use crate::update_check;

const RELEASES_URL: &str = "https://github.com/gt2rz/db-bridge/releases/download";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Tope de la descarga: los binarios ocupan unas decenas de MB.
const MAX_DOWNLOAD: u64 = 200 * 1024 * 1024;
/// Nombre del bundle dentro del `.dmg` (ver release.yml).
const MAC_APP: &str = "DB Bridge.app";
const MAC_EXECUTABLE: &str = "db-bridge";

/// Archivo de la Release para este sistema y nombre del ejecutable dentro de él. Los nombres
/// son los que publica release.yml.
fn asset() -> Option<(&'static str, &'static str)> {
    if cfg!(target_os = "macos") {
        Some(("DB-Bridge-macOS-universal.dmg", MAC_EXECUTABLE))
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(("db-bridge-linux-x86_64.tar.gz", "db-bridge"))
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        Some(("db-bridge-windows-x86_64.zip", "db-bridge.exe"))
    } else {
        None
    }
}

/// Lo que se sustituye al actualizar: el bundle `.app` en macOS y el ejecutable en el resto.
#[derive(Clone, Debug, PartialEq)]
pub enum Installation {
    MacBundle(PathBuf),
    Executable(PathBuf),
}

impl Installation {
    /// Localiza la instalación en marcha o explica por qué no se puede actualizar desde la app.
    /// Se llama al arrancar: después de sustituir el ejecutable, Linux ya no da su ruta.
    pub fn current() -> Result<Self> {
        if cfg!(debug_assertions) {
            bail!("las builds de desarrollo no se actualizan solas");
        }
        if asset().is_none() {
            bail!("no hay binarios publicados para este sistema");
        }
        let exe = std::env::current_exe().context("no se pudo localizar el ejecutable")?;
        if cfg!(target_os = "macos") {
            mac_bundle(&exe).map(Self::MacBundle)
        } else {
            Ok(Self::Executable(exe))
        }
    }

    /// Arranca la versión instalada. Quien llama cierra después esta ventana.
    pub fn relaunch(&self) -> Result<()> {
        match self {
            Self::MacBundle(bundle) => Command::new("open").arg("-n").arg(bundle).spawn(),
            Self::Executable(exe) => Command::new(exe).spawn(),
        }
        .context("no se pudo abrir la versión nueva")?;
        Ok(())
    }
}

/// El bundle que contiene `exe` (`…/DB Bridge.app/Contents/MacOS/db-bridge`).
fn mac_bundle(exe: &Path) -> Result<PathBuf> {
    let bundle = exe
        .ancestors()
        .nth(3)
        .filter(|bundle| bundle.extension() == Some(OsStr::new("app")))
        .context("DB Bridge no se está ejecutando desde un .app")?;
    // Gatekeeper ejecuta las apps en cuarentena desde una copia temporal de solo lectura.
    if bundle
        .components()
        .any(|part| part.as_os_str() == "AppTranslocation")
    {
        bail!("mueve DB Bridge a la carpeta Aplicaciones para poder actualizarla");
    }
    Ok(bundle.to_path_buf())
}

/// Descarga `version` y la instala sobre `installation`. Bloquea: se llama desde un hilo de
/// `tasks`. Si falla, la instalación actual queda como estaba.
pub fn install(installation: &Installation, version: &str) -> Result<()> {
    let (asset, executable) = asset().context("no hay binarios publicados para este sistema")?;
    let work = std::env::temp_dir().join("db-bridge").join("update");
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).with_context(|| format!("no se pudo crear {}", work.display()))?;

    let agent = update_check::agent(DOWNLOAD_TIMEOUT);
    let base = format!("{RELEASES_URL}/v{version}/{asset}");
    let checksum = agent
        .get(format!("{base}.sha256"))
        .call()
        .context("no se pudo descargar el SHA-256 de la versión nueva")?
        .body_mut()
        .read_to_string()
        .context("no se pudo leer el SHA-256 de la versión nueva")?;
    let expected = parse_checksum(&checksum)?;
    let archive = work.join(asset);
    let actual = download(&agent, &base, &archive)?;
    ensure!(
        actual == expected,
        "la descarga no coincide con su SHA-256; vuelve a intentarlo"
    );

    let result = match installation {
        Installation::MacBundle(bundle) => install_mac(&archive, &work, bundle),
        Installation::Executable(_) => {
            let binary = work.join(executable);
            if asset.ends_with(".zip") {
                extract_from_zip(&archive, executable, &binary)?;
            } else {
                extract_from_tar_gz(&archive, executable, &binary)?;
            }
            // En Windows el .exe en uso no se puede sobrescribir: self-replace lo aparta y lo
            // borra al salir. En Linux lo sustituye con un renombrado atómico.
            self_replace::self_replace(&binary).context("no se pudo sustituir el ejecutable")
        }
    };
    let _ = fs::remove_dir_all(&work);
    result
}

/// Interpreta un `.sha256` de `sha256sum`/`shasum`: `<hash>  <archivo>`.
fn parse_checksum(text: &str) -> Result<String> {
    let hash = text
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    ensure!(
        hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()),
        "el archivo .sha256 de la Release no es válido"
    );
    Ok(hash)
}

/// Guarda `url` en `path` y devuelve su SHA-256 en hexadecimal.
fn download(agent: &ureq::Agent, url: &str, path: &Path) -> Result<String> {
    let mut response = agent
        .get(url)
        .call()
        .context("no se pudo descargar la versión nueva")?;
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .reader();
    let mut file =
        File::create(path).with_context(|| format!("no se pudo crear {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .context("la descarga se interrumpió")?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])
            .with_context(|| format!("no se pudo escribir {}", path.display()))?;
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn extract_from_tar_gz(archive: &Path, name: &str, dest: &Path) -> Result<()> {
    let file =
        File::open(archive).with_context(|| format!("no se pudo abrir {}", archive.display()))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    for entry in tar.entries().context("el .tar.gz no es válido")? {
        let mut entry = entry.context("el .tar.gz no es válido")?;
        if entry.path()?.file_name() == Some(OsStr::new(name)) {
            return write_entry(&mut entry, dest);
        }
    }
    bail!("el .tar.gz no contiene {name}")
}

fn extract_from_zip(archive: &Path, name: &str, dest: &Path) -> Result<()> {
    let file =
        File::open(archive).with_context(|| format!("no se pudo abrir {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("el .zip no es válido")?;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).context("el .zip no es válido")?;
        let entry_name = entry.name().context("el .zip no es válido")?.into_owned();
        if Path::new(&entry_name).file_name() == Some(OsStr::new(name)) {
            return write_entry(&mut entry, dest);
        }
    }
    bail!("el .zip no contiene {name}")
}

fn write_entry(entry: &mut impl Read, dest: &Path) -> Result<()> {
    let mut file =
        File::create(dest).with_context(|| format!("no se pudo crear {}", dest.display()))?;
    io::copy(entry, &mut file).with_context(|| format!("no se pudo extraer {}", dest.display()))?;
    Ok(())
}

/// Copia el `.app` del `.dmg` junto al actual y los intercambia con dos renombrados en la misma
/// carpeta; si el segundo falla, se deshace el primero.
fn install_mac(dmg: &Path, work: &Path, bundle: &Path) -> Result<()> {
    let (Some(parent), Some(name)) = (bundle.parent(), bundle.file_name()) else {
        bail!("ruta de la app no válida: {}", bundle.display());
    };
    let name = name.to_string_lossy();
    let staged = parent.join(format!(".{name}.update"));
    let backup = parent.join(format!(".{name}.old"));
    remove_dir_if_exists(&staged)?;

    let mount = work.join("mnt");
    fs::create_dir_all(&mount).with_context(|| format!("no se pudo crear {}", mount.display()))?;
    run(
        "hdiutil",
        &[
            OsStr::new("attach"),
            OsStr::new("-nobrowse"),
            OsStr::new("-readonly"),
            OsStr::new("-noautoopen"),
            OsStr::new("-mountpoint"),
            mount.as_os_str(),
            dmg.as_os_str(),
        ],
    )?;
    // ditto conserva la firma, los atributos extendidos y los enlaces del bundle.
    let copied = run(
        "ditto",
        &[mount.join(MAC_APP).as_os_str(), staged.as_os_str()],
    )
    .with_context(|| format!("no se pudo escribir en {}", parent.display()));
    let _ = run(
        "hdiutil",
        &[
            OsStr::new("detach"),
            mount.as_os_str(),
            OsStr::new("-force"),
        ],
    );
    copied?;
    ensure!(
        staged.join("Contents/MacOS").join(MAC_EXECUTABLE).is_file(),
        "la versión descargada está incompleta"
    );

    remove_dir_if_exists(&backup)?;
    fs::rename(bundle, &backup)
        .with_context(|| format!("no se pudo apartar {}", bundle.display()))?;
    if let Err(error) = fs::rename(&staged, bundle) {
        let _ = fs::rename(&backup, bundle);
        let _ = fs::remove_dir_all(&staged);
        return Err(error).context("no se pudo colocar la versión nueva");
    }
    let _ = fs::remove_dir_all(&backup);
    Ok(())
}

fn remove_dir_if_exists(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("no se pudo borrar {}", path.display()))
        }
        _ => Ok(()),
    }
}

fn run(program: &str, args: &[&OsStr]) -> Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("no se pudo ejecutar {program}"))?;
    if !output.status.success() {
        bail!("{}", command_error(program, &output));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_checksum_files() {
        let hash = "A".repeat(64);
        assert_eq!(
            parse_checksum(&format!("{hash}  db-bridge-windows-x86_64.zip\r\n")).unwrap(),
            "a".repeat(64)
        );
        assert!(parse_checksum("").is_err());
        assert!(parse_checksum("1234  archivo").is_err());
        assert!(parse_checksum(&format!("{}  archivo", "z".repeat(64))).is_err());
    }

    #[test]
    fn hashes_in_lowercase_hex() {
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn finds_the_mac_bundle_of_the_executable() {
        let exe = Path::new("/Applications/DB Bridge.app/Contents/MacOS/db-bridge");
        assert_eq!(
            mac_bundle(exe).unwrap(),
            PathBuf::from("/Applications/DB Bridge.app")
        );
        assert!(mac_bundle(Path::new("/usr/local/bin/db-bridge")).is_err());
        let translocated = Path::new(
            "/private/var/folders/x/AppTranslocation/ABC/d/DB Bridge.app/Contents/MacOS/db-bridge",
        );
        assert!(mac_bundle(translocated).is_err());
    }

    /// Descarga la última Release real: `cargo test -- --ignored installs_the_published`.
    #[test]
    #[ignore = "descarga la Release publicada en GitHub"]
    fn installs_the_published_release() {
        let release = update_check::fetch_latest().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let installation = if cfg!(target_os = "macos") {
            let bundle = dir.path().join("DB Bridge.app");
            fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
            fs::write(bundle.join("Contents/MacOS/db-bridge"), b"antigua").unwrap();
            Installation::MacBundle(bundle)
        } else {
            return;
        };

        install(&installation, &release.version).unwrap();

        let Installation::MacBundle(bundle) = installation else {
            unreachable!()
        };
        let plist = fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap();
        assert!(plist.contains(&format!("<string>{}</string>", release.version)));
        assert_ne!(
            fs::read(bundle.join("Contents/MacOS/db-bridge")).unwrap(),
            b"antigua"
        );
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![OsStr::new("DB Bridge.app")]);
    }

    #[test]
    fn extracts_the_executable_from_a_tar_gz() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("release.tar.gz");
        let encoder = flate2::write::GzEncoder::new(
            File::create(&archive).unwrap(),
            flate2::Compression::default(),
        );
        let mut builder = tar::Builder::new(encoder);
        for (path, contents) in [
            ("db-bridge/README.md", &b"# DB Bridge"[..]),
            ("db-bridge/db-bridge", &b"binario"[..]),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, contents).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();

        let dest = dir.path().join("db-bridge");
        extract_from_tar_gz(&archive, "db-bridge", &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"binario");
        assert!(extract_from_tar_gz(&archive, "otro", &dest).is_err());
    }

    #[test]
    fn extracts_the_executable_from_a_zip() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("release.zip");
        let mut writer = zip::ZipWriter::new(File::create(&archive).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (path, contents) in [
            ("README.md", &b"# DB Bridge"[..]),
            ("db-bridge.exe", &b"exe"[..]),
        ] {
            writer.start_file(path, options).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap();

        let dest = dir.path().join("db-bridge.exe");
        extract_from_zip(&archive, "db-bridge.exe", &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"exe");
        assert!(extract_from_zip(&archive, "otro.exe", &dest).is_err());
    }
}
