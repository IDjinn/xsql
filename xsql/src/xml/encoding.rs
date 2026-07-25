//! Byte-level encoding handling: sniffing the encoding of an XML file (BOM,
//! UTF-16 pattern, or `<?xml ... encoding="..."?>` declaration), decoding it
//! to UTF-8 for the rest of the pipeline, and re-encoding output so a saved
//! document round-trips in its source encoding by default (`-E/--encoding`
//! overrides).

use encoding_rs::{Encoding, REPLACEMENT, UTF_8, UTF_16BE, UTF_16LE};

/// A document's on-disk encoding: which charset, and whether the file
/// carried a byte-order mark that should be written back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XmlEncoding {
    pub encoding: &'static Encoding,
    pub bom: bool,
}

impl Default for XmlEncoding {
    fn default() -> Self {
        Self { encoding: UTF_8, bom: false }
    }
}

impl XmlEncoding {
    fn is_utf16(&self) -> bool {
        self.encoding == UTF_16LE || self.encoding == UTF_16BE
    }

    /// Name to write in the `<?xml ... encoding="..."?>` declaration.
    pub fn decl_name(&self) -> &'static str {
        if self.encoding == UTF_8 {
            "utf-8"
        } else if self.is_utf16() {
            "UTF-16"
        } else {
            self.encoding.name()
        }
    }
}

/// Resolves a user-supplied `-E/--encoding` label. Accepts every WHATWG
/// label (`utf-8`, `latin1`, `windows-1252`, `shift_jis`, ...) plus
/// `utf-8-bom`/`utf8-bom` for UTF-8 with a BOM. UTF-16 output always gets a
/// BOM (a BOM-less UTF-16 file is undetectable by most tools).
pub fn from_label(label: &str) -> Option<XmlEncoding> {
    let normalized = label.trim().to_ascii_lowercase();
    if matches!(normalized.as_str(), "utf-8-bom" | "utf8-bom" | "utf-8 with bom") {
        return Some(XmlEncoding { encoding: UTF_8, bom: true });
    }
    let encoding = Encoding::for_label(normalized.as_bytes())?;
    if encoding == REPLACEMENT {
        return None;
    }
    Some(XmlEncoding { encoding, bom: encoding == UTF_16LE || encoding == UTF_16BE })
}

/// Sniffs and decodes raw file bytes to UTF-8 text. Detection order: BOM,
/// then the UTF-16 `<`-with-NUL pattern, then the XML declaration's
/// `encoding="..."` attribute, defaulting to UTF-8. Strict: bytes invalid in
/// the detected encoding are an error, not replacement characters.
pub fn decode(bytes: &[u8]) -> Result<(String, XmlEncoding), String> {
    if let Some((encoding, bom_len)) = Encoding::for_bom(bytes) {
        let text = decode_strict(encoding, &bytes[bom_len..])?;
        return Ok((text, XmlEncoding { encoding, bom: true }));
    }
    let encoding = sniff_no_bom(bytes).unwrap_or(UTF_8);
    let text = decode_strict(encoding, bytes)?;
    Ok((text, XmlEncoding { encoding, bom: false }))
}

/// Like [`decode`], but the encoding is forced (from `-E` in `--check`
/// mode). A BOM matching the forced encoding is consumed.
pub fn decode_as(enc: XmlEncoding, bytes: &[u8]) -> Result<String, String> {
    let body = match Encoding::for_bom(bytes) {
        Some((bom_enc, bom_len)) if bom_enc == enc.encoding => &bytes[bom_len..],
        _ => bytes,
    };
    decode_strict(enc.encoding, body)
}

fn decode_strict(encoding: &'static Encoding, bytes: &[u8]) -> Result<String, String> {
    encoding
        .decode_without_bom_handling_and_without_replacement(bytes)
        .map(|cow| cow.into_owned())
        .ok_or_else(|| format!("stream did not contain valid {}", encoding.name()))
}

/// BOM-less detection: UTF-16 files start `<` NUL (LE) or NUL `<` (BE);
/// otherwise the declaration is ASCII-compatible and can be scanned as bytes.
fn sniff_no_bom(bytes: &[u8]) -> Option<&'static Encoding> {
    match bytes {
        [b'<', 0, ..] => return Some(UTF_16LE),
        [0, b'<', ..] => return Some(UTF_16BE),
        _ => {}
    }
    let head = &bytes[..bytes.len().min(1024)];
    if !head.starts_with(b"<?xml") {
        return None;
    }
    let decl = &head[..head.windows(2).position(|w| w == b"?>")?];
    let pos = decl.windows(8).position(|w| w.eq_ignore_ascii_case(b"encoding"))?;
    let rest = &decl[pos + 8..];
    let quote_at = rest.iter().position(|&b| b == b'"' || b == b'\'')?;
    let quote = rest[quote_at];
    let value = &rest[quote_at + 1..];
    let end = value.iter().position(|&b| b == quote)?;
    let encoding = Encoding::for_label(&value[..end])?;
    // A decl can only say UTF-16 in a file that *is* UTF-16, which the BOM /
    // NUL checks above would have caught — trust the bytes, not the label.
    if encoding == UTF_16LE || encoding == UTF_16BE || encoding == REPLACEMENT {
        return None;
    }
    Some(encoding)
}

/// Encodes UTF-8 text into `enc`'s byte representation (BOM included when
/// the encoding carries one). Characters unmappable in the target encoding
/// become `&#NNNN;` character references, which stay valid XML.
pub fn encode(text: &str, enc: XmlEncoding) -> Vec<u8> {
    if enc.is_utf16() {
        return encode_utf16(text, enc.encoding == UTF_16BE);
    }
    if enc.encoding == UTF_8 {
        let mut out = Vec::with_capacity(text.len() + 3);
        if enc.bom {
            out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        }
        out.extend_from_slice(text.as_bytes());
        return out;
    }
    let (bytes, _, _) = enc.encoding.encode(text);
    bytes.into_owned()
}

/// encoding_rs cannot *encode* to UTF-16 (the Encoding Standard has no
/// UTF-16 output encoding), so it's done by hand. Always BOM-prefixed.
fn encode_utf16(text: &str, big_endian: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + text.len() * 2);
    out.extend_from_slice(if big_endian { &[0xFE, 0xFF] } else { &[0xFF, 0xFE] });
    for unit in text.encode_utf16() {
        let b = if big_endian { unit.to_be_bytes() } else { unit.to_le_bytes() };
        out.extend_from_slice(&b);
    }
    out
}
