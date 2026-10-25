//! COLRv0 layers and palettes through `read_fonts::model`.

use alloc::vec::Vec;
use read_fonts::model::glyph::color::{Brush, ColorGlyphVersion, CompositeMode, PaintSink};
use read_fonts::model::Font;
use read_fonts::types::{BoundingBox, GlyphId, Matrix};

/// One layer of a COLRv0 glyph: the glyph to fill and its palette entry.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Layer {
    pub glyph_id: GlyphId,
    pub color_index: Option<u16>,
}

/// Collects the layers of a COLRv0 glyph from the paint callbacks.
struct LayerSink<'a>(&'a mut Vec<Layer>);

impl PaintSink for LayerSink<'_> {
    fn push_transform(&mut self, _transform: Matrix<f32>) {}
    fn pop_transform(&mut self) {}
    fn push_clip_glyph(&mut self, _glyph: GlyphId) {}
    fn push_clip_box(&mut self, _clip_box: BoundingBox<f32>) {}
    fn pop_clip(&mut self) {}
    fn fill(&mut self, _brush: Brush<'_>) {}
    fn push_layer(&mut self, _composite_mode: CompositeMode) {}

    fn fill_glyph(
        &mut self,
        glyph: GlyphId,
        _brush_transform: Option<Matrix<f32>>,
        brush: Brush<'_>,
    ) {
        let color_index = match brush {
            Brush::Solid { palette_index, .. } if palette_index != 0xFFFF => Some(palette_index),
            _ => None,
        };
        self.0.push(Layer {
            glyph_id: glyph,
            color_index,
        });
    }
}

/// Returns true if the font has both `COLR` and `CPAL` tables.
pub(crate) fn has_color_outlines(font: &Font) -> bool {
    use read_fonts::model::Kind;
    use read_fonts::TableProvider;
    match font.kind() {
        Kind::Sfnt(tables, _) => tables.colr().is_ok() && tables.cpal().is_ok(),
        _ => false,
    }
}

/// Fills `layers` with the COLRv0 layers of `glyph_id`, in paint order.
pub(crate) fn layers(font: &Font, glyph_id: GlyphId, layers: &mut Vec<Layer>) -> bool {
    layers.clear();
    let Some(glyph) = font
        .color_glyphs()
        .get_with_version(glyph_id, ColorGlyphVersion::ColrV0)
    else {
        return false;
    };
    glyph.paint(&mut LayerSink(layers)).is_ok()
}

/// A palette, resolving entries the way swash's `ColorPalette::get` did.
pub(crate) struct Palette<'a> {
    colors: &'a [read_fonts::model::glyph::color::Color],
}

impl<'a> Palette<'a> {
    pub fn get(font: &'a Font, index: u16) -> Option<Self> {
        let palette = font.palettes().get(index as u32)?;
        Some(Self {
            colors: palette.colors(),
        })
    }

    /// Returns the color for the specified entry in RGBA order, or
    /// transparent black for an out-of-range entry.
    pub fn color(&self, index: u16) -> [u8; 4] {
        match self.colors.get(index as usize) {
            Some(c) => [c.red(), c.green(), c.blue(), c.alpha()],
            None => [0; 4],
        }
    }
}
