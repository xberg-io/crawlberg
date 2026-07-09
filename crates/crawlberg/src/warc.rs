//! WARC 1.1 (Web ARChive) output writer.
#![allow(dead_code)]
//!
//! Produces standards-compliant WARC 1.1 records for archiving crawled web content.
//! See <https://iipc.github.io/warc-specifications/specifications/warc-format/warc-1.1/>.

use std::borrow::Cow;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::error::CrawlError;

/// The WARC version header prefix written at the start of every record.
const WARC_VERSION: &str = "WARC/1.1";

/// Writer for WARC 1.1 format archive files.
///
/// Produces WARC records sequentially into a buffered file writer.
/// Each record consists of a version line, named-field headers, and an optional payload block,
/// followed by two CRLF-terminated blank lines as the record separator.
pub struct WarcWriter {
    writer: BufWriter<File>,
    warcinfo_id: Box<str>,
}

impl WarcWriter {
    /// Create a new WARC writer targeting the given file path.
    ///
    /// The file is created (or truncated) immediately but no records are written
    /// until explicit method calls.
    pub fn new(path: &Path) -> Result<Self, CrawlError> {
        let file = File::create(path).map_err(|e| CrawlError::Other(format!("create WARC file: {e}")))?;
        Ok(Self {
            writer: BufWriter::new(file),
            warcinfo_id: String::new().into_boxed_str(),
        })
    }

    /// Write a `warcinfo` record describing the software and host that produced this archive.
    ///
    /// This should be called once before writing any response records.
    /// The generated `WARC-Record-ID` is stored and referenced by subsequent records
    /// via the `WARC-Warcinfo-ID` field.
    pub fn write_warcinfo(&mut self, software: &str, hostname: &str) -> Result<(), CrawlError> {
        let record_id = make_record_id();
        let date = format_warc_date(Utc::now());

        let payload = format!("software: {software}\r\nhostname: {hostname}\r\n");
        let payload_bytes = payload.as_bytes();

        write_record(
            &mut self.writer,
            &[
                ("WARC-Type", Cow::Borrowed("warcinfo")),
                ("WARC-Date", Cow::Owned(date)),
                ("WARC-Record-ID", Cow::Borrowed(&record_id)),
                ("Content-Type", Cow::Borrowed("application/warc-fields")),
                ("Content-Length", Cow::Owned(payload_bytes.len().to_string())),
            ],
            payload_bytes,
        )?;

        self.warcinfo_id = record_id.into_boxed_str();
        Ok(())
    }

    /// Write a `response` record containing a full HTTP response (status, headers, body).
    ///
    /// Returns the `WARC-Record-ID` assigned to this record so callers can reference it.
    pub fn write_response(
        &mut self,
        url: &str,
        status: u16,
        headers: &[(&str, &str)],
        body: &[u8],
        fetch_time: DateTime<Utc>,
    ) -> Result<String, CrawlError> {
        let record_id = make_record_id();
        let date = format_warc_date(fetch_time);

        let http_block = build_http_block(status, headers, body)?;

        let mut warc_headers: Vec<(&str, Cow<'_, str>)> = vec![
            ("WARC-Type", Cow::Borrowed("response")),
            ("WARC-Date", Cow::Owned(date)),
            ("WARC-Target-URI", Cow::Borrowed(url)),
            ("WARC-Record-ID", Cow::Borrowed(&record_id)),
            ("Content-Type", Cow::Borrowed("application/http; msgtype=response")),
            ("Content-Length", Cow::Owned(http_block.len().to_string())),
        ];

        if !self.warcinfo_id.is_empty() {
            warc_headers.push(("WARC-Warcinfo-ID", Cow::Borrowed(&self.warcinfo_id)));
        }

        write_record(&mut self.writer, &warc_headers, &http_block)?;

        Ok(record_id)
    }

    /// Flush all buffered data and consume the writer.
    pub fn finish(mut self) -> Result<(), CrawlError> {
        self.writer
            .flush()
            .map_err(|e| CrawlError::Other(format!("flush WARC file: {e}")))
    }
}

/// Generate a WARC-Record-ID in the canonical `<urn:uuid:...>` format.
fn make_record_id() -> String {
    format!("<urn:uuid:{}>", Uuid::new_v4())
}

/// Format a `DateTime<Utc>` as a WARC-Date value (ISO 8601 with `Z` suffix, second precision).
fn format_warc_date(dt: DateTime<Utc>) -> String {
    let mut buf = String::with_capacity(20);
    use std::fmt::Write as _;
    write!(buf, "{}", dt.format("%Y-%m-%dT%H:%M:%SZ")).expect("write to String cannot fail");
    buf
}

/// Validate that a header name or value does not contain CR or LF characters.
fn validate_header_value(name: &str, value: &str) -> Result<(), CrawlError> {
    if name.contains('\r') || name.contains('\n') {
        return Err(CrawlError::InvalidConfig(format!(
            "header name contains invalid CR/LF characters: {name:?}"
        )));
    }
    if value.contains('\r') || value.contains('\n') {
        return Err(CrawlError::InvalidConfig(format!(
            "header value contains invalid CR/LF characters for header {name:?}"
        )));
    }
    Ok(())
}

/// Construct the HTTP response block (status-line + headers + CRLF + body) as raw bytes.
fn build_http_block(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Result<Vec<u8>, CrawlError> {
    for (name, value) in headers {
        validate_header_value(name, value)?;
    }

    let reason = http_reason_phrase(status);

    let estimated_size = 32 + headers.iter().map(|(n, v)| n.len() + v.len() + 4).sum::<usize>() + 2 + body.len();

    let mut bytes = Vec::with_capacity(estimated_size);
    use std::io::Write as _;
    write!(&mut bytes, "HTTP/1.1 {status} {reason}\r\n").expect("write to Vec cannot fail");
    for (name, value) in headers {
        write!(&mut bytes, "{name}: {value}\r\n").expect("write to Vec cannot fail");
    }
    write!(&mut bytes, "\r\n").expect("write to Vec cannot fail");
    bytes.extend_from_slice(body);
    Ok(bytes)
}

/// Write a single WARC record (version line + headers + payload + double-CRLF terminator).
fn write_record(w: &mut BufWriter<File>, headers: &[(&str, Cow<'_, str>)], payload: &[u8]) -> Result<(), CrawlError> {
    let map_io = |e: std::io::Error| CrawlError::Other(format!("write WARC record: {e}"));

    w.write_all(WARC_VERSION.as_bytes()).map_err(&map_io)?;
    w.write_all(b"\r\n").map_err(&map_io)?;

    for (name, value) in headers {
        w.write_all(name.as_bytes()).map_err(&map_io)?;
        w.write_all(b": ").map_err(&map_io)?;
        w.write_all(value.as_bytes()).map_err(&map_io)?;
        w.write_all(b"\r\n").map_err(&map_io)?;
    }

    w.write_all(b"\r\n").map_err(&map_io)?;
    w.write_all(payload).map_err(&map_io)?;
    w.write_all(b"\r\n\r\n").map_err(&map_io)?;

    Ok(())
}

/// Map common HTTP status codes to their standard reason phrase.
fn http_reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_make_record_id_format() {
        let id = make_record_id();
        assert!(id.starts_with("<urn:uuid:"));
        assert!(id.ends_with('>'));
    }

    #[test]
    fn test_format_warc_date() {
        let dt = DateTime::parse_from_rfc3339("2026-04-09T12:00:00Z")
            .expect("valid date")
            .with_timezone(&Utc);
        assert_eq!(format_warc_date(dt), "2026-04-09T12:00:00Z");
    }

    #[test]
    fn test_build_http_block() {
        let headers = vec![("Content-Type", "text/html")];
        let body = b"<html></html>";
        let block = build_http_block(200, &headers, body).expect("valid block");
        let text = String::from_utf8_lossy(&block);
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Type: text/html\r\n"));
        assert!(text.contains("\r\n\r\n<html></html>"));
    }

    #[test]
    fn test_http_reason_phrase_known() {
        assert_eq!(http_reason_phrase(200), "OK");
        assert_eq!(http_reason_phrase(404), "Not Found");
        assert_eq!(http_reason_phrase(999), "Unknown");
    }
}
