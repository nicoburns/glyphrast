//! Rendering a glyph into an [`Image`], ported from swash's `Render`.

use crate::bitmap;
use crate::color::Palette;
use crate::image::{Content, Image};
use crate::{GlyphId, Scaler, Source};
#[cfg(feature = "libm")]
use core_maths::CoreFloat;
use zeno::{Format, Mask, Origin, Point, Style, Transform, Vector};

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
    pub fn format(&mut self, format: Format) -> &mut Self {
        self.format = format;
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
                        let placement = Mask::with_scratch(outline.path(), rcx)
                            .format(self.format)
                            .origin(Origin::BottomLeft)
                            .style(self.style)
                            .offset(self.offset)
                            .render_offset(self.offset)
                            .inspect(|fmt, w, h| {
                                image.data.resize(fmt.buffer_size(w, h), 0);
                            })
                            .render_into(&mut image.data[..], None);
                        image.placement = placement;
                        image.content = if self.format == Format::Alpha {
                            Content::Mask
                        } else {
                            Content::SubpixelMask
                        };
                        image.source = Source::Outline;
                        return true;
                    }
                }
                Source::ColorOutline(palette_index) => {
                    if !scaler.has_color_outlines() {
                        continue;
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
