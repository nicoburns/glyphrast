//! Scaling and rasterization of glyphs on top of [`read_fonts`], [`skrifa`] and [`zeno`].
//!
//! This crate is a port of swash's `scale` module. The outline, image and
//! rendering types keep swash's shapes; fonts come in as a
//! [`read_fonts::model::Font`] and the hinted instances are cached
//! [`skrifa::ScalerFont`]s.
//!
//! # Building the scaler
//!
//! All scaling takes place within a [`RasterContext`], which owns the scaler
//! cache and the scratch buffers. Keep one per thread, next to your glyph
//! cache.
//!
//! ```no_run
//! # use glyphrast::{RasterContext, FontKey, Font};
//! # fn demo(font: &Font) {
//! let mut context = RasterContext::new();
//! let mut scaler = context.builder(font, FontKey(1)).size(14.).hint(true).build();
//! # }
//! ```
//!
//! # Rendering
//!
//! [`Render`] takes a prioritized list of [`Source`]s and produces an
//! [`image::Image`]:
//!
//! ```no_run
//! # use glyphrast::{RasterContext, FontKey, Font, Render, Source, StrikeWith};
//! # fn demo(font: &Font, glyph_id: u32) {
//! let mut context = RasterContext::new();
//! let mut scaler = context.builder(font, FontKey(1)).size(14.).hint(true).build();
//! let image = Render::new(&[
//!     Source::ColorOutline(0),
//!     Source::ColorBitmap(StrikeWith::BestFit),
//!     Source::Outline,
//! ])
//! .render(&mut scaler, glyph_id);
//! # }
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

pub mod image;
pub mod outline;

mod bitmap;
mod color;
mod render;
mod scaler_cache;
mod strike;

pub use read_fonts;
pub use read_fonts::model::Font;
pub use render::Render;
pub use skrifa;
pub use zeno;

use alloc::vec::Vec;
use core::borrow::Borrow;
use image::{Content, Image};
use outline::Outline;
use read_fonts::model::glyph::outline::DrawMemory;
use read_fonts::types::GlyphId as RawGlyphId;
use scaler_cache::{ScalerCache, ScalerKey};
use skrifa::{HintOptions, ScalerFont};
use smallvec::SmallVec;
use strike::Strikes;
use zeno::Placement;

/// Glyph identifier.
pub type GlyphId = u32;

/// Normalized variation coordinate in 2.14 fixed point.
pub type NormalizedCoord = i16;

/// Index of a color palette.
pub type PaletteIndex = u16;

/// Index of a bitmap strike.
pub type StrikeIndex = u32;

/// Identity of a font file, supplied by the caller.
///
/// Two [`Font`]s passed with the same key must hold the same bytes and
/// collection index. The key only selects cache entries.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct FontKey(pub u64);

/// Hinting target, matching skrifa's [`skrifa::RasterTarget`].
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum RasterTarget {
    /// Grayscale anti-aliasing.
    Normal,
    /// Lighter hinting, horizontal adjustment only.
    Light,
    /// Horizontal LCD subpixel rendering.
    Lcd,
    /// Vertical LCD subpixel rendering.
    VerticalLcd,
}

impl From<RasterTarget> for skrifa::RasterTarget {
    fn from(target: RasterTarget) -> Self {
        match target {
            RasterTarget::Normal => Self::Normal,
            RasterTarget::Light => Self::Light,
            RasterTarget::Lcd => Self::Lcd,
            RasterTarget::VerticalLcd => Self::VerticalLcd,
        }
    }
}

/// Hinting configuration for a scaler.
///
/// Every variant keeps linear metrics, so hinting never changes advances,
/// and uses skrifa's classic ClearType interpreter mode, as swash did.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum Hinting {
    /// No hinting.
    #[default]
    None,
    /// The TrueType or PostScript interpreter, falling back to the automatic
    /// hinter for TrueType fonts without `fpgm`/`prep` programs.
    AutoFallback(RasterTarget),
    /// The TrueType or PostScript interpreter only.
    Interpreter(RasterTarget),
    /// The automatic hinter only.
    Auto(RasterTarget),
}

impl Hinting {
    /// swash's configuration for `hint(true)`.
    const SWASH_DEFAULT: Self = Self::AutoFallback(RasterTarget::Lcd);

    fn options(self) -> Option<HintOptions> {
        let (options, target) = match self {
            Self::None => return None,
            Self::AutoFallback(target) => (HintOptions::auto_fallback(), target),
            Self::Interpreter(target) => (HintOptions::interpreter(), target),
            Self::Auto(target) => (HintOptions::auto(), target),
        };
        // swash's `HintingInstance` configuration corresponds to skrifa's
        // classic ClearType mode; the port keeps it for every variant.
        Some(
            options
                .with_target(target.into())
                .with_preserve_linear_metrics(true)
                .with_classic_cleartype(true),
        )
    }
}

/// Bitmap strike selection mode.
#[derive(Copy, Clone, Debug)]
pub enum StrikeWith {
    /// Load a bitmap only if the exact size is available.
    ExactSize,
    /// Load a bitmap of the best available size.
    BestFit,
    /// Loads a bitmap of the largest size available.
    LargestSize,
    /// Load a bitmap from the specified strike.
    Index(StrikeIndex),
}

/// Glyph sources for the renderer.
#[derive(Copy, Clone, Debug, Default)]
pub enum Source {
    /// Scalable outlines.
    #[default]
    Outline,
    /// Layered color scalable outlines.
    ColorOutline(PaletteIndex),
    /// Embedded alpha bitmaps.
    Bitmap(StrikeWith),
    /// Embedded color bitmaps.
    ColorBitmap(StrikeWith),
}

/// Hinted metrics reported while drawing an outline.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct OutlineMetrics {
    /// Advance width in pixels after hinting, if the scaler computed one.
    pub advance: Option<f32>,
    /// Left side bearing in pixels after hinting, if the scaler computed one.
    pub lsb: Option<f32>,
    /// True if the glyph flags overlapping contours.
    pub has_overlaps: bool,
}

/// Context that manages caches and scratch buffers for scaling.
pub struct RasterContext {
    scalers: ScalerCache,
    state: State,
}

struct State {
    scratch0: Vec<u8>,
    scratch1: Vec<u8>,
    draw_memory: Vec<u8>,
    layers: Vec<color::Layer>,
    outline: Outline,
    rcx: zeno::Scratch,
}

impl RasterContext {
    /// Creates a new context holding up to 8 configured scalers.
    pub fn new() -> Self {
        Self::with_max_entries(8)
    }

    /// Creates a new context holding up to `max_entries` configured scalers
    /// (clamped to 1..=64).
    pub fn with_max_entries(max_entries: usize) -> Self {
        Self {
            scalers: ScalerCache::new(max_entries.clamp(1, 64)),
            state: State {
                scratch0: Vec::new(),
                scratch1: Vec::new(),
                draw_memory: Vec::new(),
                layers: Vec::new(),
                outline: Outline::new(),
                rcx: zeno::Scratch::new(),
            },
        }
    }

    /// Creates a builder for a scaler of `font`, identified by `key`.
    pub fn builder<'a>(&'a mut self, font: &'a Font, key: FontKey) -> ScalerBuilder<'a> {
        ScalerBuilder {
            context: self,
            font,
            key,
            size: 0.,
            hinting: Hinting::None,
            coords: None,
        }
    }
}

impl Default for RasterContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for configuring a scaler.
pub struct ScalerBuilder<'a> {
    context: &'a mut RasterContext,
    font: &'a Font,
    key: FontKey,
    size: f32,
    hinting: Hinting,
    coords: Option<SmallVec<[read_fonts::model::NormalizedCoord; 8]>>,
}

impl<'a> ScalerBuilder<'a> {
    /// Specifies the font size in pixels per em. The default value is `0`,
    /// which produces unscaled glyphs in font units.
    pub fn size(mut self, ppem: f32) -> Self {
        self.size = ppem.max(0.);
        self
    }

    /// Specifies whether to apply hinting to outlines, with swash's
    /// configuration (interpreter with automatic fallback, LCD target,
    /// linear metrics preserved). The default value is `false`.
    pub fn hint(mut self, yes: bool) -> Self {
        self.hinting = if yes {
            Hinting::SWASH_DEFAULT
        } else {
            Hinting::None
        };
        self
    }

    /// Specifies the hinting engine and target.
    pub fn hinting(mut self, hinting: Hinting) -> Self {
        self.hinting = hinting;
        self
    }

    /// Specifies the variation location in normalized coordinates, replacing
    /// the location of the font passed to the builder.
    pub fn normalized_coords<I>(mut self, coords: I) -> Self
    where
        I: IntoIterator,
        I::Item: Borrow<NormalizedCoord>,
    {
        self.coords = Some(
            coords
                .into_iter()
                .map(|c| read_fonts::model::NormalizedCoord::from_bits(*c.borrow()))
                .collect(),
        );
        self
    }

    /// Builds a scaler for the current configuration.
    pub fn build(self) -> Scaler<'a> {
        let coords = match &self.coords {
            Some(coords) => coords.clone(),
            None => SmallVec::from_slice(self.font.normalized_coords()),
        };
        let key = ScalerKey {
            font: self.key,
            coords,
            ppem_bits: self.size.to_bits(),
            hinting: self.hinting,
        };
        let font = self.font;
        let override_coords = self.coords.as_deref();
        let scaler = self.context.scalers.get(&key, || match override_coords {
            Some(coords) => font
                .instance_builder()
                .normalized_coords(coords.iter().copied())
                .build(),
            None => font.clone(),
        });
        Scaler {
            state: &mut self.context.state,
            scaler,
            size: self.size,
        }
    }
}

/// Scales outline and bitmap glyphs.
pub struct Scaler<'a> {
    state: &'a mut State,
    scaler: &'a ScalerFont,
    size: f32,
}

impl<'a> Scaler<'a> {
    /// Returns the font instance this scaler draws from.
    pub fn font(&self) -> &'a Font {
        self.scaler.font()
    }

    /// Returns true if scalable glyph outlines are available.
    pub fn has_outlines(&self) -> bool {
        self.scaler.outline_source().is_some()
    }

    /// Scales an outline for the specified glyph into the provided outline.
    pub fn scale_outline_into(
        &mut self,
        glyph_id: GlyphId,
        outline: &mut Outline,
    ) -> Option<OutlineMetrics> {
        outline.clear();
        self.scale_outline_impl(glyph_id, None, Some(outline))
    }

    /// Scales an outline for the specified glyph.
    pub fn scale_outline(&mut self, glyph_id: GlyphId) -> Option<Outline> {
        let mut outline = Outline::new();
        self.scale_outline_into(glyph_id, &mut outline)
            .map(|_| outline)
    }

    /// Returns true if scalable color glyph outlines are available.
    pub fn has_color_outlines(&self) -> bool {
        color::has_color_outlines(self.font())
    }

    /// Scales a color outline for the specified glyph into the provided outline.
    pub fn scale_color_outline_into(&mut self, glyph_id: GlyphId, outline: &mut Outline) -> bool {
        outline.clear();
        if !self.has_color_outlines() {
            return false;
        }
        if !color::layers(
            self.font(),
            RawGlyphId::new(glyph_id),
            &mut self.state.layers,
        ) {
            return false;
        }
        for i in 0..self.state.layers.len() {
            let layer = self.state.layers[i];
            if self
                .scale_outline_impl(layer.glyph_id.to_u32(), layer.color_index, Some(outline))
                .is_none()
            {
                return false;
            }
        }
        outline.set_color(true);
        true
    }

    /// Scales a color outline for the specified glyph.
    pub fn scale_color_outline(&mut self, glyph_id: GlyphId) -> Option<Outline> {
        let mut outline = Outline::new();
        if self.scale_color_outline_into(glyph_id, &mut outline) {
            Some(outline)
        } else {
            None
        }
    }

    fn scale_outline_impl(
        &mut self,
        glyph_id: GlyphId,
        color_index: Option<u16>,
        outline: Option<&mut Outline>,
    ) -> Option<OutlineMetrics> {
        let state = &mut *self.state;
        let outline = match outline {
            Some(x) => x,
            _ => &mut state.outline,
        };
        let glyph = self.scaler.outline(RawGlyphId::new(glyph_id))?;
        outline.begin_layer(color_index);
        let needed = glyph.required_memory_size();
        if state.draw_memory.len() < needed {
            state.draw_memory.resize(needed, 0);
        }
        let metrics = glyph
            .draw(DrawMemory::Buffer(&mut state.draw_memory[..]), outline)
            .ok()?;
        outline.maybe_close();
        outline.finish();
        Some(OutlineMetrics {
            advance: metrics.h_advance,
            lsb: metrics.x_bearing,
            has_overlaps: metrics.has_overlaps,
        })
    }

    fn scale_color_outline_impl(&mut self, glyph_id: GlyphId) -> bool {
        if !self.has_color_outlines() {
            return false;
        }
        if !color::layers(
            self.font(),
            RawGlyphId::new(glyph_id),
            &mut self.state.layers,
        ) {
            return false;
        }
        self.state.outline.clear();
        for i in 0..self.state.layers.len() {
            let layer = self.state.layers[i];
            if self
                .scale_outline_impl(layer.glyph_id.to_u32(), layer.color_index, None)
                .is_none()
            {
                return false;
            }
        }
        true
    }

    /// Returns true if alpha bitmaps are available.
    pub fn has_bitmaps(&self) -> bool {
        !Strikes::alpha(self.font()).is_empty()
    }

    /// Scales a bitmap for the specified glyph and mode into the provided image.
    pub fn scale_bitmap_into(
        &mut self,
        glyph_id: GlyphId,
        strike: StrikeWith,
        image: &mut Image,
    ) -> bool {
        self.scale_bitmap_impl(glyph_id, false, strike, image) == Some(true)
    }

    /// Scales a bitmap for the specified glyph and mode.
    pub fn scale_bitmap(&mut self, glyph_id: GlyphId, strike: StrikeWith) -> Option<Image> {
        let mut image = Image::new();
        if self.scale_bitmap_into(glyph_id, strike, &mut image) {
            Some(image)
        } else {
            None
        }
    }

    /// Returns true if color bitmaps are available.
    pub fn has_color_bitmaps(&self) -> bool {
        !Strikes::color(self.font()).is_empty()
    }

    /// Scales a color bitmap for the specified glyph and mode into the provided image.
    pub fn scale_color_bitmap_into(
        &mut self,
        glyph_id: GlyphId,
        strike: StrikeWith,
        image: &mut Image,
    ) -> bool {
        self.scale_bitmap_impl(glyph_id, true, strike, image) == Some(true)
    }

    /// Scales a color bitmap for the specified glyph and mode.
    pub fn scale_color_bitmap(&mut self, glyph_id: GlyphId, strike: StrikeWith) -> Option<Image> {
        let mut image = Image::new();
        if self.scale_color_bitmap_into(glyph_id, strike, &mut image) {
            Some(image)
        } else {
            None
        }
    }

    fn scale_bitmap_impl(
        &mut self,
        glyph_id: GlyphId,
        color: bool,
        strike: StrikeWith,
        image: &mut Image,
    ) -> Option<bool> {
        image.clear();
        let size = self.size;
        let font = self.font();
        let strikes = if color {
            Strikes::color(font)
        } else {
            Strikes::alpha(font)
        };
        let glyph_id = RawGlyphId::new(glyph_id);
        let bitmap = match strike {
            StrikeWith::ExactSize => {
                if size == 0. {
                    None
                } else {
                    strikes.find_by_exact_ppem(size as u16, glyph_id)
                }
            }
            StrikeWith::BestFit => {
                if size == 0. {
                    None
                } else {
                    strikes.find_by_nearest_ppem(size as u16, glyph_id)
                }
            }
            StrikeWith::LargestSize => strikes.find_by_largest_ppem(glyph_id),
            StrikeWith::Index(i) => strikes.by_index(i, glyph_id),
        }?;
        if bitmap.ppem == 0 {
            return None;
        }
        let (_, _, bufsize) = bitmap.scaled_size(size);
        image.data.resize(bufsize, 0);
        let state = &mut *self.state;
        state.scratch0.clear();
        state.scratch1.clear();
        let mut w = bitmap.width;
        let mut h = bitmap.height;
        let scale = size / bitmap.ppem as f32;
        image.placement = if size != 0. && scale != 1. {
            state.scratch0.resize(bitmap.format.buffer_size(w, h), 0);
            w = (w as f32 * scale) as u32;
            h = (h as f32 * scale) as u32;
            image.data.resize(bitmap.format.buffer_size(w, h), 0);
            if !bitmap.decode(Some(&mut state.scratch1), &mut state.scratch0) {
                return None;
            }
            if !bitmap::resize(
                bitmap::Raster {
                    data: &state.scratch0,
                    width: bitmap.width,
                    height: bitmap.height,
                },
                bitmap.format.channels(),
                bitmap::RasterMut {
                    data: &mut image.data,
                    width: w,
                    height: h,
                },
                &mut state.scratch1,
            ) {
                return None;
            }
            let left = (bitmap.left as f32 * scale) as i32;
            let top = (bitmap.top as f32 * scale) as i32;
            Placement {
                left,
                top,
                width: w,
                height: h,
            }
        } else {
            image.data.resize(bitmap.format.buffer_size(w, h), 0);
            if !bitmap.decode(Some(&mut state.scratch1), &mut image.data) {
                return None;
            }
            Placement {
                left: bitmap.left,
                top: bitmap.top,
                width: w,
                height: h,
            }
        };
        image.source = match color {
            true => Source::ColorBitmap(strike),
            false => Source::Bitmap(strike),
        };
        image.content = match bitmap.format.channels() {
            1 => Content::Mask,
            _ => Content::Color,
        };
        Some(true)
    }
}
