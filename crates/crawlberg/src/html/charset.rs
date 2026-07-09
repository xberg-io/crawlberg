//! Character encoding detection from Content-Type headers and HTML meta tags.

/// Detect the character encoding from the Content-Type header or HTML meta tags.
pub(crate) fn detect_charset(content_type: &str, body: &str) -> Option<String> {
    if let Some(pos) = ascii_find_case_insensitive(content_type.as_bytes(), b"charset=") {
        let charset = content_type[pos + 8..]
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches('"')
            .to_lowercase();
        if !charset.is_empty() {
            return Some(charset);
        }
    }

    let search_len = body.len().min(2048);
    let search_end = (0..=search_len).rev().find(|&i| body.is_char_boundary(i)).unwrap_or(0);
    let head = &body[..search_end];
    if let Some(pos) = ascii_find_case_insensitive(head.as_bytes(), b"charset=") {
        let after = &head[pos + 8..];
        let charset = after
            .trim_start_matches(['"', '\''])
            .split(|c: char| c == '"' || c == '\'' || c == '>' || c == ';' || c.is_whitespace())
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if !charset.is_empty() {
            return Some(charset);
        }
    }

    if body.starts_with('\u{FEFF}') {
        return Some("utf-8".to_owned());
    }

    None
}

/// Case-insensitive search for an ASCII needle within a byte slice.
fn ascii_find_case_insensitive(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let needle_lower: Vec<u8> = needle.iter().map(|b| b.to_ascii_lowercase()).collect();
    'outer: for i in 0..=(haystack.len() - needle.len()) {
        for (j, &nb) in needle_lower.iter().enumerate() {
            if haystack[i + j].to_ascii_lowercase() != nb {
                continue 'outer;
            }
        }
        return Some(i);
    }
    None
}
