pub mod font;
mod layout;

use crate::presentation::{DisplayLayout, Panel};

/// A 1-bit pixel buffer representing what the e-ink display shows.
///
/// Pixels are packed 8 per byte, row-major. Bit 7 of each byte is the
/// leftmost pixel in the group. A set bit (1) is black; clear (0) is white.
#[derive(Debug, Clone)]
pub struct PixelBuffer {
    pub width: u32,
    pub height: u32,
    /// Raw pixel data: ceil(width * height / 8) bytes.
    pub pixels: Vec<u8>,
}

impl PixelBuffer {
    /// Create a new all-white (zero) pixel buffer.
    pub fn new(width: u32, height: u32) -> Self {
        let byte_count = (width * height).div_ceil(8) as usize;
        PixelBuffer {
            width,
            height,
            pixels: vec![0u8; byte_count],
        }
    }

    /// Reset all pixels to white.
    pub fn clear(&mut self) {
        self.pixels.fill(0);
    }

    /// Set a single pixel. `true` = black, `false` = white.
    pub fn set_pixel(&mut self, x: u32, y: u32, black: bool) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = (y * self.width + x) as usize;
        let byte = idx / 8;
        let bit = 7 - (idx % 8);
        if black {
            self.pixels[byte] |= 1 << bit;
        } else {
            self.pixels[byte] &= !(1 << bit);
        }
    }

    /// Get a single pixel value.
    pub fn get_pixel(&self, x: u32, y: u32) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        let idx = (y * self.width + x) as usize;
        let byte = idx / 8;
        let bit = 7 - (idx % 8);
        (self.pixels[byte] >> bit) & 1 == 1
    }

    /// Encode this pixel buffer as a PNG image (grayscale).
    ///
    /// Black pixels (1) become 0x00 (black), white pixels (0) become 0xFF (white).
    pub fn to_png(&self) -> Vec<u8> {
        use image::{GrayImage, Luma};

        let mut img = GrayImage::new(self.width, self.height);
        for y in 0..self.height {
            for x in 0..self.width {
                let luma = if self.get_pixel(x, y) { 0u8 } else { 255u8 };
                img.put_pixel(x, y, Luma([luma]));
            }
        }

        let mut png_bytes = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut png_bytes);
        image::ImageEncoder::write_image(
            encoder,
            img.as_raw(),
            self.width,
            self.height,
            image::ColorType::L8,
        )
        .expect("PNG encoding should not fail for valid dimensions");
        png_bytes
    }
}

/// Render a list of panels into a 1-bit PixelBuffer (800x480).
///
/// This is the single public entry point for the render pipeline.
/// It lays out panels in a grid, draws borders and title bars, and
/// rasterizes text using the embedded bitmap font.
pub fn render(panels: &[Panel]) -> PixelBuffer {
    let mut buf = PixelBuffer::new(800, 480);
    layout::layout_and_render(panels, &mut buf);
    buf
}

/// Render a list of panels into a PixelBuffer with explicit dimensions.
///
/// Kept for backward compatibility with Wave 1 callers.
pub fn render_panels(panels: &[Panel], width: u32, height: u32) -> PixelBuffer {
    let mut buf = PixelBuffer::new(width, height);
    layout::layout_and_render(panels, &mut buf);
    buf
}

/// Render a typed [`DisplayLayout`] into a 800×480 1-bit pixel buffer.
///
/// This is the primary render entry point for the new 4-zone e-ink display design.
pub fn render_display(layout: &DisplayLayout) -> PixelBuffer {
    let mut buf = PixelBuffer::new(800, 480);
    layout::layout_and_render_display(layout, &mut buf);
    buf
}

/// Render a startup/loading screen for the 800×480 e-ink display.
///
/// Use this for the initial display update on first boot, before any data sources
/// have completed their first fetch. Transitions to the live layout when data arrives.
pub fn render_startup() -> PixelBuffer {
    let mut buf = PixelBuffer::new(800, 480);
    layout::layout_and_render_startup(&mut buf);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::Panel;

    #[test]
    fn new_buffer_is_all_white() {
        let buf = PixelBuffer::new(800, 480);
        assert_eq!(buf.width, 800);
        assert_eq!(buf.height, 480);
        assert!(buf.pixels.iter().all(|&b| b == 0));
    }

    #[test]
    fn buffer_byte_count() {
        // 800 × 480 = 384 000 pixels → 48 000 bytes
        let buf = PixelBuffer::new(800, 480);
        assert_eq!(buf.pixels.len(), 48_000);
    }

    #[test]
    fn set_and_get_pixel() {
        let mut buf = PixelBuffer::new(16, 1);
        assert!(!buf.get_pixel(0, 0));
        buf.set_pixel(0, 0, true);
        assert!(buf.get_pixel(0, 0));
        buf.set_pixel(0, 0, false);
        assert!(!buf.get_pixel(0, 0));
    }

    #[test]
    fn out_of_bounds_set_is_noop() {
        let mut buf = PixelBuffer::new(8, 8);
        buf.set_pixel(100, 100, true); // should not panic
    }

    #[test]
    fn clear_resets_pixels() {
        let mut buf = PixelBuffer::new(8, 8);
        buf.set_pixel(3, 3, true);
        assert!(buf.get_pixel(3, 3));
        buf.clear();
        assert!(!buf.get_pixel(3, 3));
        assert!(buf.pixels.iter().all(|&b| b == 0));
    }

    #[test]
    fn to_png_produces_valid_png() {
        let buf = PixelBuffer::new(8, 8);
        let png = buf.to_png();
        // PNG magic bytes
        assert_eq!(&png[..4], &[0x89, b'P', b'N', b'G']);
    }

    #[test]
    fn render_empty_panels() {
        let buf = render(&[]);
        assert_eq!(buf.width, 800);
        assert_eq!(buf.height, 480);
        // All white — no panels means nothing drawn
        assert!(buf.pixels.iter().all(|&b| b == 0));
    }

    #[test]
    fn render_single_panel_has_pixels() {
        let panel = Panel::new("Test").with_row("Hello World");
        let buf = render(&[panel]);
        // Should have some black pixels (borders + text)
        assert!(buf.pixels.iter().any(|&b| b != 0));
    }

    #[test]
    fn render_multiple_panels() {
        let panels = vec![
            Panel::new("Weather").with_row("72F Sunny"),
            Panel::new("River").with_row("5.2 ft"),
            Panel::new("Ferry").with_row("Next: 10:30"),
        ];
        let buf = render(&panels);
        assert!(buf.pixels.iter().any(|&b| b != 0));
    }

    #[test]
    fn render_panels_returns_correct_dimensions() {
        let buf = render_panels(&[], 800, 480);
        assert_eq!(buf.width, 800);
        assert_eq!(buf.height, 480);
    }

    #[test]
    fn render_startup_has_correct_dimensions() {
        let buf = render_startup();
        assert_eq!(buf.width, 800);
        assert_eq!(buf.height, 480);
    }

    #[test]
    fn render_startup_has_pixels() {
        // Startup screen should draw header text and loading message — not blank.
        let buf = render_startup();
        assert!(buf.pixels.iter().any(|&b| b != 0));
    }

    #[test]
    fn render_startup_has_header_divider() {
        // Header divider is drawn at y=28–29.
        let buf = render_startup();
        assert!(buf.get_pixel(0, 28));
    }
}
