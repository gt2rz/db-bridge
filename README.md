# DB Bridge

Inspector SQLite de escritorio para desarrollo de apps **offline-first**. Conecta a un archivo local o extrae la base de datos de un simulador de iOS / emulador de Android, permítete consultar y editar celdas, y devuelve los cambios al dispositivo.

Construido en Rust con `eframe`/`egui` (UI nativa, sin WebView) y `rusqlite` (SQLite embebido con la feature `bundled`, sin dependencias del sistema).

## Funciones

- Archivo local: diálogo nativo o arrastrar un `.db`/`.sqlite` a la ventana.
- iOS: extrae la BD del simulador encendido detectando la ruta real dentro del contenedor de la app.
- Android: extrae la BD del emulador/dispositivo con `run-as` (requiere APK debuggable).
- Selector de dispositivo cuando hay varios simuladores encendidos o varios dispositivos/emuladores en `adb` (`-s <serial>` / UDID). Se recuerda entre sesiones y en las conexiones recientes.
- Detección automática de apps instaladas (filtra las apps de sistema) y de las bases de datos dentro de cada app.
- Tamaño en disco de la BD conectada (y de su `-wal`, si lo hay) en la tarjeta de conexión.
- Navegación de tablas y vistas con indicador de solo lectura.
- Paginación de tablas y vistas en páginas de 200 filas, con el total de filas y controles de primera/anterior/siguiente/última. La página se conserva al refrescar y se ajusta si la tabla encoge.
- Pantalla de bienvenida con selección de origen y conexiones recientes.
- Editor SQL multilínea con resaltado de sintaxis, números de línea y `Ctrl/Cmd + Enter`.
- Edición de celdas con doble clic, `Enter` para guardar y `Esc` para cancelar.
- `Live Watch`: re-extrae y re-ejecuta la consulta cada 2 segundos.
- Comparación con una línea base: **Fijar línea base** guarda el estado actual y **Ver cambios** muestra, por tabla, las filas insertadas, eliminadas y modificadas (celda a celda, `antes → después`), además de tablas nuevas, eliminadas o con columnas distintas. Con Live Watch la comparación se actualiza en cada extracción.
- `Push automático` (activado por defecto): cada cambio, sea una edición de celda o un `INSERT`/`UPDATE`/`DELETE`/DDL desde el editor, se envía al simulador/dispositivo en cuanto se aplica.
- `Push al emulador`: devuelve la BD editada al simulador/dispositivo a mano, por ejemplo con el push automático desactivado.
- Exportación de resultados a CSV (RFC 4180) y JSON (conserva números y `null`).
- Consultas favoritas persistentes entre sesiones.
- Soporte de WAL: copia `-wal` y `-shm`, y hace checkpoint antes de cada push.
- Detección, extracción y push en segundo plano: la ventana sigue respondiendo mientras trabajan `adb` o `simctl`, con un indicador en la barra de estado.

## Requisitos

- Rust estable ≥ 1.95 (`rustup` lo instala con el `rust-toolchain.toml` del repo).
- macOS con Xcode para el modo iOS (`xcrun simctl`).
- `adb` en el `PATH` para el modo Android (Android SDK Platform Tools).
- APK compilado en modo **debug** para poder usar `run-as` en Android.

## Instalación

```bash
cargo build --release
./target/release/db-bridge
```

Para tenerlo disponible en todo el sistema:

```bash
cargo install --path .
db-bridge
```

## Uso

### Archivo local

1. Pulsa **Abrir archivo SQLite...** o arrastra la BD a la ventana.
2. Selecciona una tabla en el panel izquierdo o escribe una consulta.
3. Doble clic en una celda para editarla.

### Simulador iOS

1. Arranca el simulador y abre tu app al menos una vez para que cree la BD.
2. Elige **Simulador iOS** en el selector de origen.
3. Pulsa **Detectar**, elige el simulador si hay más de uno encendido, tu bundle ID y la base de datos del desplegable (se detectan por la cabecera `SQLite format 3`, así que también aparecen los `.store` de SwiftData/Core Data). Si no aparece, escribe el nombre del archivo.
4. Pulsa **Extraer y conectar**.

La herramienta busca el archivo en `Documents`, `Library/Application Support`, `Library`, `Library/Caches` y `tmp`, y si no aparece recorre el contenedor con un límite de profundidad.

### Emulador / dispositivo Android

1. Conecta el dispositivo o arranca el emulador (`adb devices` debe listarlo).
2. Elige **Emulador Android**.
3. Pulsa **Detectar**, elige el dispositivo si hay más de uno conectado y el package, o escríbelo a mano y vuelve a pulsar **Detectar**.
4. Elige la base de datos del desplegable: se listan los archivos de `databases/` y los `.db`, `.db3`, `.sqlite` y `.sqlite3` del resto del contenedor. También puedes escribir el nombre: `databases/` se busca primero y luego todo el contenedor con `find`.
5. Pulsa **Extraer y conectar**.

Con un archivo local los cambios se escriben directamente en el archivo. Con un simulador/dispositivo se aplican primero en una copia local; con **Push automático** activo se envían en el acto, y si lo desactivas aparece «Cambios sin push» hasta que pulses **Push al emulador**. En Android se ejecuta `am force-stop`, se copia el archivo a `/data/local/tmp` y se mueve al contenedor con `run-as`; en iOS se cierra la app con `simctl terminate`. Vuelve a abrir la app para ver los cambios.

> La copia de ida y vuelta se hace con `adb exec-out`/`adb push`, nunca con `adb shell cat > archivo`, para no corromper binarios por la traducción de fin de línea de la PTY.

## Arquitectura

```
src/
├── main.rs        # bootstrap de eframe y ventana
├── app.rs         # estado de la UI y todos los paneles
├── model.rs       # SourceMode, Settings, FavoriteQuery
├── db.rs          # apertura, esquema, consultas, edición, export y tests
├── diff.rs        # línea base y comparación de cambios por tabla
├── favorites.rs   # persistencia de ajustes y favoritos
├── theme.rs       # paleta, fuentes del sistema y estilos de egui
├── sql_highlight.rs # resaltado de sintaxis del editor SQL
├── tasks.rs       # operaciones lentas en hilos aparte
└── mobile/
    ├── mod.rs     # API común (pull/push/detect) y validación de entradas
    ├── adb.rs     # Android
    └── ios.rs     # iOS
```

Decisiones relevantes:

- La interfaz usa un tema oscuro propio (`theme.rs`). No se distribuye ninguna fuente: se cargan en tiempo de ejecución las del sistema (SF Pro/SF Mono en macOS, Segoe UI/Cascadia Mono o Consolas en Windows, Noto o DejaVu en Linux) y, si no existen, las que trae egui.

- Los nombres de tabla y columna se interpolan siempre con `quote_ident`, que duplica comillas dobles.
- Las tablas normales se editan por `rowid`; las `WITHOUT ROWID` con clave primaria simple usan esa columna como clave. Vistas y claves compuestas quedan en solo lectura.
- Detección, pull y push se lanzan en hilos (`tasks.rs`) y devuelven el resultado por un canal; los resultados que llegan después de cambiar de modo o de app se descartan. El push envía una copia instantánea (`push/` junto a la copia local), así que se puede seguir editando mientras se envía; los cambios que llegan con un push en curso se envían en otro al terminar. Mientras dura un pull no se ejecutan consultas ni ediciones, porque la copia local se está sobrescribiendo.
- La copia local recuerda el dispositivo, la app y el archivo de los que se extrajo: el push, Refrescar y Live Watch usan siempre ese origen, aunque después se cambien los selectores.
- Las páginas de una tabla se ordenan por su clave (`rowid` o la PK de las `WITHOUT ROWID`) para que no se solapen; las vistas y las tablas sin clave usan el orden de SQLite. Las consultas escritas a mano en el editor no se paginan: se ejecutan tal cual.
- La línea base es una copia byte a byte del archivo y su `-wal` (no `VACUUM INTO`, que puede renumerar `rowid`). La comparación adjunta la copia con `ATTACH` y cruza las filas por clave primaria (o `rowid`); si la clave cambió entre versiones, compara filas completas con `EXCEPT` y no puede distinguir modificaciones. Se muestran hasta 200 filas por tabla y tipo de cambio; los contadores son exactos.
- Los valores se mantienen tipados (`Null`, `Integer`, `Real`, `Text`, `Blob`) hasta el render y la exportación.
- Los archivos extraídos se guardan en `$TMPDIR/db-bridge/{ios,android}/<app>/`, eliminando WAL/SHM obsoletos antes de cada extracción.
- Los ajustes, favoritos y conexiones recientes se guardan en `~/Library/Application Support/db-bridge/settings.json` (o el directorio de configuración equivalente del sistema).
- Los identificadores de app y nombres de archivo se validan contra una lista blanca para no inyectar argumentos en las llamadas a `adb`/`xcrun`. El archivo puede ser un nombre o una ruta relativa al contenedor (nunca absoluta ni con `..`); con ruta, pull y push usan ese archivo exacto aunque haya otro con el mismo nombre. Los espacios solo se admiten en iOS, donde no interviene ningún shell.

## Rutas típicas por framework

| Framework | iOS | Android |
| --- | --- | --- |
| Flutter (`sqflite`, `drift`) | `Documents` o `Library/Application Support` | `databases/` |
| React Native (`op-sqlite`, WatermelonDB) | `Documents` | `databases/` |
| Room / SQLDelight | — | `databases/` |
| SwiftData / Core Data | `Library/Application Support` (el archivo suele ser `.store`) | — |

La autodetección recorre el contenedor, así que también funciona si tu app guarda la BD en una subcarpeta propia. Para SwiftData escribe el nombre real del archivo (por ejemplo `default.store`).

## WAL y consistencia

Las apps offline-first suelen usar `PRAGMA journal_mode=WAL`. Por eso:

- Al extraer se copian `archivo.db`, `archivo.db-wal` y `archivo.db-shm`. Al abrir la copia, SQLite aplica el WAL pendiente.
- Al editar, los cambios pueden quedar en el `-wal` de la copia temporal. Antes de un push se ejecuta `PRAGMA wal_checkpoint(TRUNCATE)` para consolidarlos en el archivo principal.
- Copiar una BD mientras la app está escribiendo puede producir un snapshot mixto. Para datos sensibles al tiempo, cierra la app antes de extraer (o usa `Push`, que ya detiene la app).

## Solución de problemas

| Mensaje | Causa | Solución |
| --- | --- | --- |
| `no se pudo ejecutar adb` | `adb` no está en el `PATH` | Añade `$ANDROID_HOME/platform-tools` al `PATH`. |
| `no se pudo extraer ... ¿La app está compilada en modo debug?` | APK release o nombre incorrecto | Instala una build debug; revisa el nombre del archivo. |
| `no se encontró ... en el simulador` | Simulador apagado o bundle ID incorrecto | Arranca el simulador y confirma el bundle ID con **Detectar**. |
| `no se encontró «archivo» dentro del contenedor` | La app aún no creó la BD o usa otro nombre | Abre la app en el simulador y revisa el nombre/ruta. |
| `Error SQL: no such table` | La consulta apunta a otra BD | Pulsa **Refrescar** o revisa la tabla seleccionada. |
| `Solo lectura` | Vista, `WITHOUT ROWID` con PK compuesta o consulta manual | Edita la tabla base o escribe una consulta sobre ella. |
| `database is locked` | La app mantiene bloqueos | Usa **Push** (detiene la app) o ciérrala antes de extraer. |

## Desarrollo

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

Las pruebas cubren listado de tablas/vistas, consultas editables, `WITHOUT ROWID`, comillas en identificadores, exportación CSV/JSON, validación de entradas, el resaltado SQL y el recorrido de directorios de iOS.

## Limitaciones conocidas

- No hay edición de esquema (DDL asistido) ni migraciones.
- El editor ejecuta una sentencia por vez.
- Los BLOB se muestran y exportan como `<BLOB n bytes>`.
- Las tablas con clave primaria compuesta son de solo lectura.
- El refresh de `Live Watch` sustituye la copia temporal: si hay cambios sin push, se pausa para no perderlos.
