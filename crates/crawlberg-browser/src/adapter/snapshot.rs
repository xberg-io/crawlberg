//! Pixel-level PNG rendering for native browser screenshots.

use crate::page::Page;

pub(super) const SCREENSHOT_VIEWPORT_WIDTH: u32 = 1280;
pub(super) const SCREENSHOT_VIEWPORT_HEIGHT: u32 = 720;
pub(super) const MAX_NATIVE_SCREENSHOT_HEIGHT: u32 = 16_384;
const RGBA_CHANNELS: usize = 4;
const SNAPSHOT_MARGIN: u32 = 24;
const SNAPSHOT_ROW_HEIGHT: u32 = 18;
const SNAPSHOT_ROW_GAP: u32 = 6;
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

/// A rectangular region of the snapshot canvas, in pixels.
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

pub(super) fn screenshot_content_height(page: &mut Page, html: &str) -> u32 {
    let dom_height = page
        .evaluate_result("document.documentElement && document.documentElement.scrollHeight")
        .ok()
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(SCREENSHOT_VIEWPORT_HEIGHT);
    dom_height
        .max(snapshot_content_height(html))
        .max(css_pixel_height_hint(html).unwrap_or(SCREENSHOT_VIEWPORT_HEIGHT))
}

pub(super) fn render_snapshot_png(html: &str, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut buffer = vec![255; width as usize * height as usize * RGBA_CHANNELS];
    draw_snapshot_background(&mut buffer, width, height);
    draw_snapshot_rows(&mut buffer, width, height, html);
    encode_png(&buffer, width, height)
}

fn draw_snapshot_background(buffer: &mut [u8], width: u32, height: u32) {
    for y in 0..height {
        for x in 0..width {
            let offset = pixel_offset(width, x, y);
            let shade = if y < 56 { 238 } else { 250 };
            buffer[offset] = shade;
            buffer[offset + 1] = shade;
            buffer[offset + 2] = shade;
            buffer[offset + 3] = 255;
        }
    }
}

fn draw_snapshot_rows(buffer: &mut [u8], width: u32, height: u32, html: &str) {
    let mut y = SNAPSHOT_MARGIN;
    for chunk in snapshot_chunks(html) {
        if y + SNAPSHOT_ROW_HEIGHT >= height {
            break;
        }
        let row_width = snapshot_row_width(width, &chunk);
        let color = snapshot_color(&chunk);
        let rect = Rect {
            x: SNAPSHOT_MARGIN,
            y,
            width: row_width,
            height: SNAPSHOT_ROW_HEIGHT,
        };
        fill_rect(buffer, width, rect, color);
        y += SNAPSHOT_ROW_HEIGHT + SNAPSHOT_ROW_GAP;
    }
}

fn snapshot_content_height(html: &str) -> u32 {
    let row_count = u32::try_from(snapshot_chunks(html).len()).unwrap_or(u32::MAX);
    SNAPSHOT_MARGIN
        .saturating_mul(2)
        .saturating_add(row_count.saturating_mul(SNAPSHOT_ROW_HEIGHT.saturating_add(SNAPSHOT_ROW_GAP)))
}

fn snapshot_chunks(html: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut text = String::new();
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => {
                push_snapshot_text(&mut chunks, &mut text);
                in_tag = true;
            }
            '>' => {
                in_tag = false;
            }
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    push_snapshot_text(&mut chunks, &mut text);
    if chunks.is_empty() {
        chunks.push("empty document".to_string());
    }
    chunks
}

fn css_pixel_height_hint(html: &str) -> Option<u32> {
    let mut rest = html;
    let mut height = None;
    while let Some(index) = rest.find("height:") {
        rest = &rest[index + "height:".len()..];
        let candidate = parse_css_pixel_value(rest);
        height = height.max(candidate);
    }
    height
}

fn parse_css_pixel_value(input: &str) -> Option<u32> {
    let trimmed = input.trim_start();
    let number: String = trimmed.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    if number.is_empty() {
        return None;
    }
    let suffix = trimmed[number.len()..].trim_start();
    if !suffix.starts_with("px") {
        return None;
    }
    number.parse().ok()
}

fn push_snapshot_text(chunks: &mut Vec<String>, text: &mut String) {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !normalized.is_empty() {
        chunks.push(normalized);
    }
    text.clear();
}

fn snapshot_row_width(width: u32, text: &str) -> u32 {
    let max_width = width.saturating_sub(SNAPSHOT_MARGIN * 2);
    let text_width = (text.chars().count() as u32).saturating_mul(9).max(48);
    text_width.min(max_width)
}

fn snapshot_color(text: &str) -> [u8; 4] {
    let bytes = stable_hash64(text).to_le_bytes();
    [
        80_u8.saturating_add(bytes[0] / 3),
        96_u8.saturating_add(bytes[1] / 3),
        112_u8.saturating_add(bytes[2] / 3),
        255,
    ]
}

fn stable_hash64(text: &str) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn fill_rect(buffer: &mut [u8], canvas_width: u32, rect: Rect, color: [u8; 4]) {
    for row in rect.y..rect.y + rect.height {
        for col in rect.x..rect.x + rect.width {
            let offset = pixel_offset(canvas_width, col, row);
            buffer[offset..offset + RGBA_CHANNELS].copy_from_slice(&color);
        }
    }
}

fn pixel_offset(width: u32, x: u32, y: u32) -> usize {
    (y as usize * width as usize + x as usize) * RGBA_CHANNELS
}

fn encode_png(buffer: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("failed to write native screenshot PNG header: {e}"))?;
        writer
            .write_image_data(buffer)
            .map_err(|e| format!("failed to write native screenshot PNG data: {e}"))?;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_snapshot_png_produces_decodable_rgba_image_with_expected_background_and_row_pixels() {
        let width = 100;
        let height = 100;
        let png_bytes = render_snapshot_png("<p>hi</p>", width, height).expect("render_snapshot_png should succeed");

        assert_eq!(
            &png_bytes[..8],
            b"\x89PNG\r\n\x1a\n",
            "output must start with the PNG signature"
        );

        let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes.as_slice()));
        let mut reader = decoder.read_info().expect("PNG should decode");
        let mut buffer = vec![0_u8; reader.output_buffer_size().expect("PNG should report a buffer size")];
        let info = reader.next_frame(&mut buffer).expect("PNG frame should decode");
        assert_eq!(info.width, width);
        assert_eq!(info.height, height);
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);

        let below_header_offset = pixel_offset(width, 0, height - 1);
        assert_eq!(
            &buffer[below_header_offset..below_header_offset + RGBA_CHANNELS],
            &[250, 250, 250, 255],
            "rows below the header band should use the lighter background shade"
        );

        let header_offset = pixel_offset(width, 0, 0);
        assert_eq!(
            &buffer[header_offset..header_offset + RGBA_CHANNELS],
            &[238, 238, 238, 255],
            "rows within the header band should use the darker background shade"
        );

        let text_row_offset = pixel_offset(width, SNAPSHOT_MARGIN, SNAPSHOT_MARGIN);
        let expected_color = snapshot_color("hi");
        assert_eq!(
            &buffer[text_row_offset..text_row_offset + RGBA_CHANNELS],
            &expected_color,
            "first text row should be filled with its deterministic snapshot color"
        );
    }

    #[test]
    fn snapshot_content_height_scales_with_chunk_count() {
        let empty_height = snapshot_content_height("");
        assert_eq!(
            empty_height,
            SNAPSHOT_MARGIN * 2 + (SNAPSHOT_ROW_HEIGHT + SNAPSHOT_ROW_GAP)
        );

        let two_chunk_height = snapshot_content_height("<p>one</p><p>two</p>");
        assert_eq!(
            two_chunk_height,
            SNAPSHOT_MARGIN * 2 + 2 * (SNAPSHOT_ROW_HEIGHT + SNAPSHOT_ROW_GAP)
        );
    }

    #[test]
    fn css_pixel_height_hint_extracts_largest_px_height_declaration() {
        assert_eq!(css_pixel_height_hint("<div style=\"height: 300px\">"), Some(300));
        assert_eq!(
            css_pixel_height_hint("<div style=\"height:120px\"></div><div style=\"height: 900px\">"),
            Some(900)
        );
        assert_eq!(css_pixel_height_hint("<div>no height here</div>"), None);
    }
}
