use crate::theme;
use eframe::egui::{
    Color32, FontId,
    text::{LayoutJob, TextFormat},
};
use std::ops::Range;

const KEYWORDS: &[&str] = &[
    "ABORT",
    "ADD",
    "ALL",
    "ALTER",
    "AND",
    "AS",
    "ASC",
    "ATTACH",
    "AUTOINCREMENT",
    "BEGIN",
    "BETWEEN",
    "BY",
    "CASE",
    "CAST",
    "CHECK",
    "COLLATE",
    "COLUMN",
    "COMMIT",
    "CONFLICT",
    "CONSTRAINT",
    "CREATE",
    "CROSS",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DETACH",
    "DISTINCT",
    "DROP",
    "ELSE",
    "END",
    "ESCAPE",
    "EXCEPT",
    "EXISTS",
    "EXPLAIN",
    "FALSE",
    "FOREIGN",
    "FROM",
    "FULL",
    "GLOB",
    "GROUP",
    "HAVING",
    "IF",
    "IGNORE",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "ISNULL",
    "JOIN",
    "KEY",
    "LEFT",
    "LIKE",
    "LIMIT",
    "NOT",
    "NOTNULL",
    "NULL",
    "OFFSET",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PARTITION",
    "PRAGMA",
    "PRIMARY",
    "QUERY",
    "RECURSIVE",
    "REFERENCES",
    "REINDEX",
    "RELEASE",
    "RENAME",
    "REPLACE",
    "RETURNING",
    "RIGHT",
    "ROLLBACK",
    "ROWID",
    "SAVEPOINT",
    "SELECT",
    "SET",
    "TABLE",
    "TEMP",
    "THEN",
    "TO",
    "TRANSACTION",
    "TRIGGER",
    "TRUE",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VACUUM",
    "VALUES",
    "VIEW",
    "WHEN",
    "WHERE",
    "WINDOW",
    "WITH",
    "WITHOUT",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    Identifier,
    QuotedIdentifier,
    String,
    Number,
    Comment,
    Punctuation,
    Whitespace,
}

/// Divide el texto en tramos con su tipo. Los tramos cubren todo el texto, en orden y sin huecos,
/// así que una consulta a medio escribir (comillas sin cerrar, etc.) también se puede pintar.
pub fn tokenize(text: &str) -> Vec<(Range<usize>, TokenKind)> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut start = 0;

    while start < bytes.len() {
        let byte = bytes[start];
        let (end, kind) = if byte.is_ascii_whitespace() {
            (
                scan_while(bytes, start, |b| b.is_ascii_whitespace()),
                TokenKind::Whitespace,
            )
        } else if text[start..].starts_with("--") {
            (
                text[start..]
                    .find('\n')
                    .map_or(bytes.len(), |offset| start + offset),
                TokenKind::Comment,
            )
        } else if text[start..].starts_with("/*") {
            (
                text[start + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |offset| start + 2 + offset + 2),
                TokenKind::Comment,
            )
        } else if byte == b'\'' {
            (scan_quoted(bytes, start, b'\''), TokenKind::String)
        } else if byte == b'"' || byte == b'`' {
            (scan_quoted(bytes, start, byte), TokenKind::QuotedIdentifier)
        } else if byte == b'[' {
            (
                text[start..]
                    .find(']')
                    .map_or(bytes.len(), |offset| start + offset + 1),
                TokenKind::QuotedIdentifier,
            )
        } else if byte.is_ascii_digit() {
            (
                scan_while(bytes, start, |b| b.is_ascii_alphanumeric() || b == b'.'),
                TokenKind::Number,
            )
        } else if is_word_byte(byte) {
            let end = scan_while(bytes, start, is_word_byte);
            let kind = if is_keyword(&text[start..end]) {
                TokenKind::Keyword
            } else {
                TokenKind::Identifier
            };
            (end, kind)
        } else {
            let width = text[start..].chars().next().map_or(1, char::len_utf8);
            (start + width, TokenKind::Punctuation)
        };
        tokens.push((start..end, kind));
        start = end;
    }

    tokens
}

pub fn layout_job(text: &str, font_id: FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    for (range, kind) in tokenize(text) {
        let format = TextFormat {
            italics: kind == TokenKind::Comment,
            ..TextFormat::simple(font_id.clone(), color(kind))
        };
        job.append(&text[range], 0.0, format);
    }
    job
}

fn color(kind: TokenKind) -> Color32 {
    match kind {
        TokenKind::Keyword => theme::keyword(),
        TokenKind::QuotedIdentifier => theme::ident(),
        TokenKind::String => theme::string(),
        TokenKind::Number => theme::number(),
        TokenKind::Comment => theme::subtle(),
        TokenKind::Punctuation => theme::muted(),
        TokenKind::Identifier | TokenKind::Whitespace => theme::text(),
    }
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 0x80
}

fn is_keyword(word: &str) -> bool {
    KEYWORDS
        .iter()
        .any(|keyword| keyword.eq_ignore_ascii_case(word))
}

fn scan_while(bytes: &[u8], start: usize, keep: impl Fn(u8) -> bool) -> usize {
    bytes[start..]
        .iter()
        .position(|&b| !keep(b))
        .map_or(bytes.len(), |offset| start + offset)
}

/// Avanza hasta la comilla de cierre; una comilla doblada (`''`) es un escape.
fn scan_quoted(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == quote {
            if bytes.get(index + 1) == Some(&quote) {
                index += 2;
                continue;
            }
            return index + 1;
        }
        index += 1;
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<(&str, TokenKind)> {
        tokenize(text)
            .into_iter()
            .filter(|(_, kind)| *kind != TokenKind::Whitespace)
            .map(|(range, kind)| (&text[range], kind))
            .collect()
    }

    #[test]
    fn classifies_basic_query() {
        assert_eq!(
            kinds("select \"user id\", name FROM users WHERE age >= 18;"),
            vec![
                ("select", TokenKind::Keyword),
                ("\"user id\"", TokenKind::QuotedIdentifier),
                (",", TokenKind::Punctuation),
                ("name", TokenKind::Identifier),
                ("FROM", TokenKind::Keyword),
                ("users", TokenKind::Identifier),
                ("WHERE", TokenKind::Keyword),
                ("age", TokenKind::Identifier),
                (">", TokenKind::Punctuation),
                ("=", TokenKind::Punctuation),
                ("18", TokenKind::Number),
                (";", TokenKind::Punctuation),
            ]
        );
    }

    #[test]
    fn handles_escaped_quotes_and_comments() {
        assert_eq!(
            kinds("SELECT 'O''Brien' -- autor\n/* fin */"),
            vec![
                ("SELECT", TokenKind::Keyword),
                ("'O''Brien'", TokenKind::String),
                ("-- autor", TokenKind::Comment),
                ("/* fin */", TokenKind::Comment),
            ]
        );
    }

    #[test]
    fn covers_unterminated_input_without_gaps() {
        let text = "SELECT 'sin cerrar ñ";
        let tokens = tokenize(text);
        let mut cursor = 0;
        for (range, _) in &tokens {
            assert_eq!(range.start, cursor);
            cursor = range.end;
        }
        assert_eq!(cursor, text.len());
        assert_eq!(
            tokens.last().map(|(_, kind)| *kind),
            Some(TokenKind::String)
        );
    }
}
