// text.rs — rasterize up to two ASCII lines into a 16px-tall LUMINANCE buffer
// (row 0-7 = top line, row 8-15 = bottom line) via the font8x8 bitmap font.
use font8x8::{UnicodeFonts, BASIC_FONTS};

pub const HUD_COLS: usize = 96; // max chars/line
pub const HUD_W: usize = HUD_COLS * 8; // 768
pub const HUD_H: usize = 16; // two 8px lines

pub fn render_two(top: &str, bottom: &str, buf: &mut [u8]) {
    for b in buf.iter_mut() {
        *b = 0;
    }
    blit(top, 0, buf);
    blit(bottom, 8, buf);
}

fn blit(s: &str, y_off: usize, buf: &mut [u8]) {
    for (ci, ch) in s.chars().enumerate() {
        if ci >= HUD_COLS {
            break;
        }
        let x0 = ci * 8;
        if let Some(glyph) = BASIC_FONTS.get(ch) {
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..8usize {
                    if bits & (1u8 << col) != 0 {
                        buf[(y_off + row) * HUD_W + x0 + col] = 255;
                    }
                }
            }
        }
    }
}
