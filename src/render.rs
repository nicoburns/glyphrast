//! Rendering a glyph into an [`Image`], ported from swash's `Render`.

use crate::bitmap;
use crate::color::Palette;
use crate::colr1;
use crate::image::{Content, Image};
use crate::{lcd, GlyphId, Scaler, Source};
#[cfg(feature = "libm")]
use core_maths::CoreFloat;
use read_fonts::model::glyph::color::ColorGlyphVersion;
use read_fonts::types::GlyphId as RawGlyphId;
use zeno::{Mask, Origin, Point, Style, Transform, Vector};

/// Target format for rasterizing an outline.
///
/// The first three variants are zeno's formats and convert from
/// [`zeno::Format`]; [`Format::Lcd`] is this crate's filtered LCD mask.
#[derive(Copy, Clone, PartialEq, Debug, Default)]
pub enum Format {
    /// 8-bit alpha mask.
    #[default]
    Alpha,
    /// 32-bit RGBA subpixel mask as zeno renders it: three passes offset by
    /// -0.3, 0 and +0.3 pixels, unfiltered, with alpha left at zero.
    Subpixel,
    /// zeno's subpixel mask with custom offsets for each channel.
    CustomSubpixel([f32; 3]),
    /// 32-bit RGBA LCD mask: one pass at three samples per pixel, FIR
    /// filtered across subpixels as FreeType does, with alpha set to the mean
    /// of the three channels. A [`zeno::Stroke`] with `scale: false` is
    /// widened in subpixels rather than pixels.
    Lcd {
        /// Filter applied across neighbouring subpixels.
        filter: LcdFilter,
        /// Order of the subpixels within a pixel.
        order: SubpixelOrder,
    },
}

impl Format {
    /// LCD mask with the default filter and RGB subpixel order.
    pub const LCD: Self = Self::Lcd {
        filter: LcdFilter::Default,
        order: SubpixelOrder::Rgb,
    };
}

impl From<zeno::Format> for Format {
    fn from(format: zeno::Format) -> Self {
        match format {
            zeno::Format::Alpha => Self::Alpha,
            zeno::Format::Subpixel => Self::Subpixel,
            zeno::Format::CustomSubpixel(offsets) => Self::CustomSubpixel(offsets),
        }
    }
}

/// FIR filter applied across the subpixels of a [`Format::Lcd`] mask, with
/// FreeType's weights.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum LcdFilter {
    /// FreeType's `FT_LCD_FILTER_DEFAULT`: `[0x08, 0x4D, 0x56, 0x4D, 0x08]`.
    #[default]
    Default,
    /// FreeType's `FT_LCD_FILTER_LIGHT`: `[0x00, 0x55, 0x56, 0x55, 0x00]`.
    Light,
    /// No filtering; each channel holds the raw coverage of its subpixel.
    None,
}

/// Order of the colour stripes within a pixel of a [`Format::Lcd`] mask.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub enum SubpixelOrder {
    /// Red, green, blue from left to right.
    #[default]
    Rgb,
    /// Blue, green, red from left to right.
    Bgr,
}

/// Builder type for rendering a glyph into an image.
///
/// See the crate level [documentation](crate#rendering) for detail.
pub struct Render<'a> {
    sources: &'a [Source],
    format: Format,
    offset: Point,
    transform: Option<Transform>,
    embolden: f32,
    foreground: [u8; 4],
    style: Style<'a>,
}

impl<'a> Render<'a> {
    /// Creates a new builder for configuring rendering using the specified
    /// prioritized list of sources.
    pub fn new(sources: &'a [Source]) -> Self {
        Self {
            sources,
            format: Format::Alpha,
            offset: Point::new(0., 0.),
            transform: None,
            embolden: 0.,
            foreground: [128, 128, 128, 255],
            style: Style::default(),
        }
    }

    /// Specifies the target format for rasterizing an outline. Default is
    /// [`Format::Alpha`].
    pub fn format(&mut self, format: impl Into<Format>) -> &mut Self {
        self.format = format.into();
        self
    }

    /// Specifies the path style to use when rasterizing an outline. Default is
    /// [`Fill::NonZero`](zeno::Fill::NonZero).
    pub fn style(&mut self, style: impl Into<Style<'a>>) -> &mut Self {
        self.style = style.into();
        self
    }

    /// Specifies an additional offset to apply when rasterizing an outline.
    /// Default is `(0, 0)`.
    pub fn offset(&mut self, offset: Vector) -> &mut Self {
        self.offset = offset;
        self
    }

    /// Specifies a transformation matrix to apply when rasterizing an
    /// outline. Default is `None`.
    pub fn transform(&mut self, transform: Option<Transform>) -> &mut Self {
        self.transform = transform;
        self
    }

    /// Specifies the strength of a faux bold transform to apply when
    /// rasterizing an outline. Default is `0`.
    pub fn embolden(&mut self, strength: f32) -> &mut Self {
        self.embolden = strength;
        self
    }

    /// Specifies an RGBA color to use when rasterizing layers of a color
    /// outline that do not directly reference a palette color. Default is
    /// `[128, 128, 128, 255]`.
    pub fn default_color(&mut self, color: [u8; 4]) -> &mut Self {
        self.foreground = color;
        self
    }

    /// Renders the specified glyph using the current configuration into the
    /// provided image.
    pub fn render_into(&self, scaler: &mut Scaler, glyph_id: GlyphId, image: &mut Image) -> bool {
        for source in self.sources {
            match source {
                Source::Outline => {
                    if !scaler.has_outlines() {
                        continue;
                    }
                    scaler.state.outline.clear();
                    if scaler.scale_outline_impl(glyph_id, None, None).is_some() {
                        let state = &mut *scaler.state;
                        let rcx = &mut state.rcx;
                        let outline = &mut state.outline;
                        if self.embolden != 0. {
                            outline.embolden(self.embolden, self.embolden);
                        }
                        if let Some(transform) = &self.transform {
                            outline.transform(transform);
                        }
                        image.content = match self.format {
                            Format::Lcd { filter, order } => {
                                // One alpha pass at three samples per pixel,
                                // then filter and pack into pixels.
                                let coverage = &mut state.scratch0;
                                let offset = Vector::new(self.offset.x * 3., self.offset.y);
                                coverage.clear();
                                let placement = Mask::with_scratch(outline.path(), rcx)
                                    .transform(Some(Transform::scale(3., 1.)))
                                    .origin(Origin::BottomLeft)
                                    .style(self.style)
                                    .offset(offset)
                                    .render_offset(offset)
                                    .inspect(|fmt, w, h| {
                                        coverage.resize(fmt.buffer_size(w, h), 0);
                                    })
                                    .render_into(&mut coverage[..], None);
                                image.placement = lcd::pack(
                                    coverage,
                                    placement,
                                    filter,
                                    order,
                                    &mut state.scratch1,
                                    &mut image.data,
                                );
                                Content::SubpixelMask
                            }
                            format => {
                                let zeno_format = match format {
                                    Format::Alpha => zeno::Format::Alpha,
                                    Format::Subpixel => zeno::Format::Subpixel,
                                    Format::CustomSubpixel(offsets) => {
                                        zeno::Format::CustomSubpixel(offsets)
                                    }
                                    Format::Lcd { .. } => unreachable!(),
                                };
                                let placement = Mask::with_scratch(outline.path(), rcx)
                                    .format(zeno_format)
                                    .origin(Origin::BottomLeft)
                                    .style(self.style)
                                    .offset(self.offset)
                                    .render_offset(self.offset)
                                    .inspect(|fmt, w, h| {
                                        image.data.resize(fmt.buffer_size(w, h), 0);
                                    })
                                    .render_into(&mut image.data[..], None);
                                image.placement = placement;
                                if format == Format::Alpha {
                                    Content::Mask
                                } else {
                                    Content::SubpixelMask
                                }
                            }
                        };
                        image.source = Source::Outline;
                        return true;
                    }
                }
                Source::ColorOutline(palette_index) => {
                    if !scaler.has_color_outlines() {
                        continue;
                    }
                    let font = scaler.font();
                    if let Some(glyph) = font
                        .color_glyphs()
                        .get_with_version(RawGlyphId::new(glyph_id), ColorGlyphVersion::ColrV1)
                    {
                        let state = &mut *scaler.state;
                        let buffers = colr1::Buffers {
                            outline: &mut state.outline,
                            draw_memory: &mut state.draw_memory,
                            rcx: &mut state.rcx,
                            scratch: &mut state.colr1,
                        };
                        let palette = Palette::get(font, *palette_index);
                        if colr1::Painter::new(scaler.scaler, buffers).render(
                            &glyph,
                            palette,
                            self.foreground,
                            self.offset,
                            self.transform,
                            image,
                        ) {
                            image.source = Source::ColorOutline(*palette_index);
                            image.content = Content::Color;
                            return true;
                        }
                    }
                    scaler.state.outline.clear();
                    if scaler.scale_color_outline_impl(glyph_id) {
                        let font = scaler.font();
                        let state = &mut *scaler.state;
                        let scratch = &mut state.scratch0;
                        let rcx = &mut state.rcx;
                        let outline = &mut state.outline;
                        if let Some(transform) = &self.transform {
                            outline.transform(transform);
                        }
                        let palette = Palette::get(font, *palette_index);

                        let total_bounds = outline.bounds();

                        // need to take offset into account when placing glyph
                        let base_x = (total_bounds.min.x + self.offset.x).floor() as i32;
                        let base_y = (total_bounds.min.y + self.offset.y).ceil() as i32;
                        let base_w = total_bounds.width().ceil() as u32;
                        let base_h = total_bounds.height().ceil() as u32;

                        image.data.resize((base_w * base_h * 4) as usize, 0);
                        image.placement.left = base_x;
                        image.placement.top = base_h as i32 + base_y;
                        image.placement.width = total_bounds.width().ceil() as u32;
                        image.placement.height = total_bounds.height().ceil() as u32;

                        let mut ok = true;
                        for i in 0..outline.len() {
                            let layer = match outline.get(i) {
                                Some(layer) => layer,
                                _ => {
                                    ok = false;
                                    break;
                                }
                            };

                            scratch.clear();
                            let placement = Mask::with_scratch(layer.path(), rcx)
                                .origin(Origin::BottomLeft)
                                .style(self.style)
                                .offset(self.offset)
                                .render_offset(self.offset)
                                .inspect(|fmt, w, h| {
                                    scratch.resize(fmt.buffer_size(w, h), 0);
                                })
                                .render_into(&mut scratch[..], None);
                            let color = layer
                                .color_index()
                                .and_then(|i| palette.as_ref().map(|p| p.color(i)))
                                .unwrap_or(self.foreground);
                            bitmap::blit(
                                &scratch[..],
                                placement.width,
                                placement.height,
                                placement.left.wrapping_sub(base_x),
                                (base_h as i32 + base_y).wrapping_sub(placement.top),
                                color,
                                &mut image.data,
                                base_w,
                                base_h,
                            );
                        }
                        if ok {
                            image.source = Source::ColorOutline(*palette_index);
                            image.content = Content::Color;
                            return true;
                        }
                    }
                }
                Source::Bitmap(mode) => {
                    if !scaler.has_bitmaps() {
                        continue;
                    }
                    if scaler.scale_bitmap_into(glyph_id, *mode, image) {
                        return true;
                    }
                }
                Source::ColorBitmap(mode) => {
                    if !scaler.has_color_bitmaps() {
                        continue;
                    }
                    if scaler.scale_color_bitmap_into(glyph_id, *mode, image) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Renders the specified glyph using the current configuration.
    pub fn render(&self, scaler: &mut Scaler, glyph_id: GlyphId) -> Option<Image> {
        let mut image = Image::new();
        if self.render_into(scaler, glyph_id, &mut image) {
            Some(image)
        } else {
            None
        }
    }
}
