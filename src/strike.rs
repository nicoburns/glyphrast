//! Embedded bitmap lookup with swash's strike selection and placement rules,
//! reading the tables through `read_fonts::model`.

use alloc::vec::Vec;
#[cfg(feature = "libm")]
use core_maths::CoreFloat;
use read_fonts::model::glyph::bitmap::{BitmapGlyph, BitmapSource, BitmapStrikes, Pixels, Strike};
use read_fonts::model::glyph::GlyphSourceOrder;
use read_fonts::model::Font;
use read_fonts::types::{GlyphId, NameId};

/// Pixel format of an embedded bitmap, as swash classified it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum BitmapFormat {
    /// Byte-aligned alpha mask with the specified number of bits.
    Alpha(u8),
    /// Bit-aligned alpha mask with the specified number of bits.
    Packed(u8),
    /// 32-bit color, copied as stored in the font.
    Color,
    /// PNG encoded.
    Png,
}

impl BitmapFormat {
    pub fn channels(&self) -> u32 {
        match self {
            Self::Alpha(..) | Self::Packed(..) => 1,
            _ => 4,
        }
    }

    pub fn buffer_size(&self, width: u32, height: u32) -> usize {
        (width * height * self.channels()) as usize
    }
}

/// An embedded bitmap glyph with swash's placement conventions.
#[derive(Clone)]
pub(crate) struct Bitmap<'a> {
    pub format: BitmapFormat,
    pub ppem: u16,
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    pub pixels: Pixels<'a>,
}

impl<'a> Bitmap<'a> {
    fn new(glyph: &BitmapGlyph<'a>, is_sbix: bool, is_apple: bool, upem: u16) -> Self {
        let pixels = glyph.pixels();
        let format = match &pixels {
            Pixels::Png(_) => BitmapFormat::Png,
            Pixels::Bgra(_) => BitmapFormat::Color,
            Pixels::Mask(mask) => {
                if mask.is_packed() {
                    BitmapFormat::Packed(mask.bits_per_pixel())
                } else {
                    BitmapFormat::Alpha(mask.bits_per_pixel())
                }
            }
        };
        let ppem = glyph.ppem();
        let (width, height) = (glyph.width(), glyph.height());
        let left = glyph.inner_x_bearing();
        let mut top = glyph.inner_y_bearing();
        if is_sbix {
            // swash: `if top == 0 && is_apple { top = (-100 * ppem / upem).round() }`
            if top == 0 && is_apple {
                let s = ppem as f32 / upem as f32;
                top = (-100. * s).round() as i32;
            }
            top += height as i32;
        }
        Self {
            format,
            ppem,
            width,
            height,
            left,
            top,
            pixels,
        }
    }

    pub fn decoded_size(&self) -> usize {
        self.format.buffer_size(self.width, self.height)
    }

    pub fn scaled_size(&self, size: f32) -> (u32, u32, usize) {
        let mut w = self.width;
        let mut h = self.height;
        if size != 0. {
            let scale = size / self.ppem as f32;
            w = (w as f32 * scale) as u32;
            h = (h as f32 * scale) as u32;
        }
        (w, h, self.format.buffer_size(w, h))
    }

    /// Decodes the bitmap into the specified target buffer.
    pub fn decode(&self, scratch: Option<&mut Vec<u8>>, target: &mut [u8]) -> bool {
        let mut tmp = Vec::new();
        let scratch = if let Some(scratch) = scratch {
            scratch
        } else {
            &mut tmp
        };
        let size = self.decoded_size();
        if target.len() < size {
            return false;
        }
        let w = self.width as usize;
        let h = self.height as usize;
        let dst = &mut target[..size];
        match &self.pixels {
            Pixels::Png(png) => {
                scratch.clear();
                if super::bitmap::decode_png(png, scratch, dst).is_none() {
                    return false;
                }
            }
            Pixels::Bgra(bgra) => {
                let src = bgra.data();
                if src.len() < size {
                    return false;
                }
                dst.copy_from_slice(&src[..size]);
            }
            Pixels::Mask(mask) => {
                let src = mask.data();
                let bits = mask.bits_per_pixel() as usize;
                if mask.is_packed() {
                    if src.len() < (w * h * bits).div_ceil(8) {
                        return false;
                    }
                    match bits {
                        1 => {
                            for x in 0..(w * h) {
                                dst[x] = (src[x >> 3] >> (!x & 7) & 1) * 255;
                            }
                        }
                        2 => {
                            for x in 0..(w * h) {
                                dst[x] = (src[x >> 2] >> (!(x * 2) & 2) & 3) * 85;
                            }
                        }
                        4 => {
                            for x in 0..(w * h) {
                                dst[x] = (src[x >> 1] >> (!(x * 4) & 4) & 15) * 17;
                            }
                        }
                        8 => dst.copy_from_slice(&src[..size]),
                        _ => return false,
                    }
                } else {
                    let row_bytes = (w * bits).div_ceil(8);
                    if src.len() < row_bytes * h {
                        return false;
                    }
                    match bits {
                        1 => {
                            let mut dst_idx = 0;
                            for row in src[..row_bytes * h].chunks(row_bytes) {
                                for x in 0..w {
                                    dst[dst_idx] = (row[x >> 3] >> (!x & 7) & 1) * 255;
                                    dst_idx += 1;
                                }
                            }
                        }
                        2 => {
                            let mut dst_idx = 0;
                            for row in src[..row_bytes * h].chunks(row_bytes) {
                                for x in 0..w {
                                    dst[dst_idx] = (row[x >> 2] >> (!(x * 2) & 2) & 3) * 85;
                                    dst_idx += 1;
                                }
                            }
                        }
                        4 => {
                            let mut dst_idx = 0;
                            for row in src[..row_bytes * h].chunks(row_bytes) {
                                for x in 0..w {
                                    dst[dst_idx] = (row[x >> 1] >> (!(x * 4) & 4) & 15) * 17;
                                    dst_idx += 1;
                                }
                            }
                        }
                        8 => dst.copy_from_slice(&src[..size]),
                        _ => return false,
                    }
                }
            }
        }
        true
    }
}

/// The strikes of one kind (alpha or color) in a font.
pub(crate) struct Strikes<'a> {
    strikes: BitmapStrikes<'a>,
    is_sbix: bool,
    is_apple: bool,
    upem: u16,
}

impl<'a> Strikes<'a> {
    pub fn alpha(font: &'a Font) -> Self {
        Self::new(
            font,
            font.bitmap_strikes_with(GlyphSourceOrder::MONO_BITMAPS),
        )
    }

    pub fn color(font: &'a Font) -> Self {
        Self::new(
            font,
            font.bitmap_strikes_with(GlyphSourceOrder::COLOR_BITMAPS),
        )
    }

    fn new(font: &'a Font, strikes: BitmapStrikes<'a>) -> Self {
        let is_sbix = strikes.source() == Some(BitmapSource::Sbix);
        let is_apple = is_sbix && is_apple_color_emoji(font);
        Self {
            strikes,
            is_sbix,
            is_apple,
            upem: font.units_per_em(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.strikes.source().is_none()
    }

    fn len(&self) -> u32 {
        self.strikes.len()
    }

    fn strike(&self, index: u32) -> Option<Strike<'a>> {
        self.strikes.get(index)
    }

    fn glyph(&self, strike: &Strike<'a>, glyph_id: GlyphId) -> Option<Bitmap<'a>> {
        strike
            .get(glyph_id)
            .map(|g| Bitmap::new(&g, self.is_sbix, self.is_apple, self.upem))
    }

    /// Nearest suitable size, preferring larger strikes when there is no exact match.
    pub fn find_by_nearest_ppem(&self, ppem: u16, glyph_id: GlyphId) -> Option<Bitmap<'a>> {
        let mut best = None;
        for i in 0..self.len() {
            let Some(strike) = self.strike(i) else {
                continue;
            };
            let Some(bitmap) = self.glyph(&strike, glyph_id) else {
                continue;
            };
            // swash kept the last strike that contains the glyph, and returned
            // the first one at least as large as the request.
            let strike_ppem = strike.ppem();
            if strike_ppem >= ppem {
                return Some(bitmap);
            }
            best = Some(bitmap);
        }
        best
    }

    pub fn find_by_exact_ppem(&self, ppem: u16, glyph_id: GlyphId) -> Option<Bitmap<'a>> {
        for i in 0..self.len() {
            let Some(strike) = self.strike(i) else {
                continue;
            };
            let Some(bitmap) = self.glyph(&strike, glyph_id) else {
                continue;
            };
            if strike.ppem() == ppem {
                return Some(bitmap);
            }
        }
        None
    }

    pub fn find_by_largest_ppem(&self, glyph_id: GlyphId) -> Option<Bitmap<'a>> {
        let mut largest = None;
        let mut largest_ppem = 0;
        for i in 0..self.len() {
            let Some(strike) = self.strike(i) else {
                continue;
            };
            let Some(bitmap) = self.glyph(&strike, glyph_id) else {
                continue;
            };
            let strike_ppem = strike.ppem();
            if largest.is_none() || strike_ppem > largest_ppem {
                largest = Some(bitmap);
                largest_ppem = strike_ppem;
            }
        }
        largest
    }

    pub fn by_index(&self, index: u32, glyph_id: GlyphId) -> Option<Bitmap<'a>> {
        let strike = self.strike(index)?;
        self.glyph(&strike, glyph_id)
    }
}

fn is_apple_color_emoji(font: &Font) -> bool {
    font.names()
        .get(NameId::FAMILY_NAME)
        .and_then(|name| name.english_or_first())
        .map(|name| name.name().chars().eq("Apple Color Emoji".chars()))
        .unwrap_or(false)
}
