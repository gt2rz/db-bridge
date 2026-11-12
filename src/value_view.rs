//! Presentación de valores largos o binarios: JSON formateado, volcado hexadecimal y tipo de
//! archivo de un BLOB.

/// Bytes que se vuelcan como máximo en el visor; el resto se indica con el total.
pub const HEX_DUMP_LIMIT: usize = 4096;
/// Caracteres que se muestran de un texto en la rejilla de resultados.
pub const CELL_PREVIEW_CHARS: usize = 80;

/// El texto formateado si es un objeto o un array JSON válido.
pub fn pretty_json(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    serde_json::to_string_pretty(&value).ok()
}

/// Tipo de archivo reconocible por su cabecera.
pub fn blob_kind(bytes: &[u8]) -> Option<&'static str> {
    const SIGNATURES: &[(&[u8], &str)] = &[
        (b"\x89PNG\r\n\x1a\n", "imagen PNG"),
        (b"\xFF\xD8\xFF", "imagen JPEG"),
        (b"GIF87a", "imagen GIF"),
        (b"GIF89a", "imagen GIF"),
        (b"%PDF-", "documento PDF"),
        (b"SQLite format 3\0", "base de datos SQLite"),
        (b"\x1F\x8B", "gzip"),
        (b"PK\x03\x04", "zip"),
        (b"bplist00", "property list binaria"),
    ];
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("imagen WebP");
    }
    SIGNATURES
        .iter()
        .find(|(signature, _)| bytes.starts_with(signature))
        .map(|(_, kind)| *kind)
}

/// Extensión sugerida al guardar un BLOB reconocido.
pub fn blob_extension(bytes: &[u8]) -> &'static str {
    match blob_kind(bytes) {
        Some("imagen PNG") => "png",
        Some("imagen JPEG") => "jpg",
        Some("imagen GIF") => "gif",
        Some("imagen WebP") => "webp",
        Some("documento PDF") => "pdf",
        Some("base de datos SQLite") => "db",
        Some("gzip") => "gz",
        Some("zip") => "zip",
        Some("property list binaria") => "plist",
        _ => "bin",
    }
}

/// Volcado de 16 bytes por línea: desplazamiento, hexadecimal y ASCII imprimible.
pub fn hex_dump(bytes: &[u8]) -> String {
    let mut out = String::new();
    for (line, chunk) in bytes.chunks(16).take(HEX_DUMP_LIMIT / 16).enumerate() {
        let hex = chunk
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let ascii: String = chunk
            .iter()
            .map(|&byte| {
                if byte.is_ascii_graphic() || byte == b' ' {
                    byte as char
                } else {
                    '.'
                }
            })
            .collect();
        out.push_str(&format!("{:08x}  {hex:<47}  {ascii}\n", line * 16));
    }
    if bytes.len() > HEX_DUMP_LIMIT {
        out.push_str(&format!("… {} bytes más\n", bytes.len() - HEX_DUMP_LIMIT));
    }
    out
}

/// Primera línea de un texto, recortada para caber en una celda. `None` si cabe entero.
pub fn cell_preview(text: &str) -> Option<String> {
    let first_line = text.lines().next().unwrap_or("");
    let multiline = first_line.len() < text.trim_end().len();
    let mut chars = first_line.chars();
    let preview: String = chars.by_ref().take(CELL_PREVIEW_CHARS).collect();
    if chars.next().is_some() || multiline {
        Some(format!("{preview}…"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_only_json_objects_and_arrays() {
        assert_eq!(
            pretty_json(r#"{"a":1,"b":[true,null]}"#).unwrap(),
            "{\n  \"a\": 1,\n  \"b\": [\n    true,\n    null\n  ]\n}"
        );
        assert!(pretty_json("[1,2]").is_some());
        assert!(pretty_json("42").is_none());
        assert!(pretty_json("{no es json").is_none());
        assert!(pretty_json("texto").is_none());
    }

    #[test]
    fn recognizes_common_file_headers() {
        assert_eq!(blob_kind(b"\x89PNG\r\n\x1a\n...."), Some("imagen PNG"));
        assert_eq!(blob_kind(b"RIFF\0\0\0\0WEBPVP8 "), Some("imagen WebP"));
        assert_eq!(blob_kind(b"%PDF-1.7"), Some("documento PDF"));
        assert_eq!(blob_kind(&[0, 1, 2]), None);
        assert_eq!(blob_extension(b"\xFF\xD8\xFF\xE0"), "jpg");
        assert_eq!(blob_extension(&[0, 1, 2]), "bin");
    }

    #[test]
    fn hex_dump_shows_offset_hex_and_ascii() {
        let dump = hex_dump(b"Hola\x00\x01 mundo!!!!!!xyz");
        let lines: Vec<_> = dump.lines().collect();
        assert_eq!(
            lines[0],
            "00000000  48 6f 6c 61 00 01 20 6d 75 6e 64 6f 21 21 21 21  Hola.. mundo!!!!"
        );
        assert!(lines[1].starts_with("00000010  21 21 78 79 7a"));
        assert!(lines[1].ends_with("!!xyz"));
    }

    #[test]
    fn hex_dump_is_limited() {
        let dump = hex_dump(&vec![0u8; HEX_DUMP_LIMIT + 10]);
        assert_eq!(dump.lines().count(), HEX_DUMP_LIMIT / 16 + 1);
        assert!(dump.ends_with("… 10 bytes más\n"));
    }

    #[test]
    fn previews_long_or_multiline_text() {
        assert_eq!(cell_preview("corto"), None);
        assert_eq!(
            cell_preview("línea 1\nlínea 2").as_deref(),
            Some("línea 1…")
        );
        let long = "x".repeat(CELL_PREVIEW_CHARS + 5);
        assert_eq!(
            cell_preview(&long),
            Some(format!("{}…", "x".repeat(CELL_PREVIEW_CHARS)))
        );
    }
}
