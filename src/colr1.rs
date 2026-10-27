//! COLRv1 paint graphs, composited in software into a premultiplied RGBA
//! image.
//!
//! The paint callbacks arrive in font units under a transform stack. The
//! painter keeps that stack in font units and maps every shape and brush
//! into pixels with one matrix, so hinted outlines from the scaler and
//! gradients evaluated per pixel agree.

use crate::color::Palette;
use crate::image::Image;
use crate::outline::Outline;
use alloc::vec::Vec;
#[cfg(feature = "libm")]
use core_maths::CoreFloat;
use read_fonts::model::glyph::color::{
    Brush, ColorGlyph, ColorStop, CompositeMode, Extend, PaintSink,
};
use read_fonts::model::glyph::outline::DrawMemory;
use read_fonts::types::{BoundingBox, GlyphId, Matrix, Point};
use skrifa::ScalerFont;
use zeno::{Command, Mask, Origin, PathData, Placement, Transform, Vector};

/// Largest canvas the painter renders, in pixels.
const MAX_PIXELS: usize = 1 << 24;

/// Buffers reused across COLRv1 renders.
#[derive(Default)]
pub(crate) struct Scratch {
    /// Stack of `w * h * 4` premultiplied RGBA layers; the first is the output.
    layers: Vec<u8>,
    frames: Vec<Frame>,
    /// Stack of `w * h` coverage masks, each already intersected with the
    /// clips below it.
    clips: Vec<u8>,
    clip_count: usize,
    /// Transform stack in font units.
    transforms: Vec<Matrix<f32>>,
    /// Coverage of the shape most recently rasterized.
    coverage: Vec<u8>,
    stops: Vec<Stop>,
}

struct Frame {
    mode: CompositeMode,
    /// Clip stack depth when the layer was pushed.
    clip: usize,
}

#[derive(Copy, Clone)]
struct Stop {
    offset: f32,
    /// Straight alpha, 0 to 1.
    color: [f32; 4],
}

/// Per-context state the painter borrows from the scaler.
pub(crate) struct Buffers<'a> {
    pub outline: &'a mut Outline,
    pub draw_memory: &'a mut Vec<u8>,
    pub rcx: &'a mut zeno::Scratch,
    pub scratch: &'a mut Scratch,
}

/// A [`PaintSink`] that composites into a pixel canvas.
pub(crate) struct Painter<'a> {
    scaler: &'a ScalerFont,
    buffers: Buffers<'a>,
    palette: Option<Palette<'a>>,
    foreground: [u8; 4],
    /// Font units to pixels, including the caller's transform and offset.
    to_px: Matrix<f32>,
    /// Pixels per font unit of the scaler's outlines.
    scale: f32,
    /// Canvas origin (left, bottom) in pixels, y up.
    x0: i32,
    y0: i32,
    width: usize,
    height: usize,
}

impl<'a> Painter<'a> {
    pub fn new(scaler: &'a ScalerFont, buffers: Buffers<'a>) -> Self {
        Self {
            scaler,
            buffers,
            palette: None,
            foreground: [0; 4],
            to_px: Matrix::IDENTITY,
            scale: 1.,
            x0: 0,
            y0: 0,
            width: 0,
            height: 0,
        }
    }

    /// Renders `glyph` into `image`. Returns false if the glyph is malformed
    /// or too large, leaving `image` unspecified.
    pub fn render(
        mut self,
        glyph: &ColorGlyph<'_>,
        palette: Option<Palette<'a>>,
        foreground: [u8; 4],
        offset: Vector,
        transform: Option<Transform>,
        image: &mut Image,
    ) -> bool {
        let upem = self.scaler.font().units_per_em() as f32;
        if upem <= 0. {
            return false;
        }
        self.scale = self
            .scaler
            .ppem()
            .filter(|ppem| *ppem > 0.)
            .map_or(1., |ppem| ppem / upem);
        let mut user = transform.map_or(Matrix::IDENTITY, from_zeno);
        user.dx += offset.x;
        user.dy += offset.y;
        self.to_px = user * scaled(self.scale);
        self.palette = palette;
        self.foreground = foreground;

        let Ok(extents) = glyph.extents() else {
            return false;
        };
        let corners = [
            (extents.x_bearing, extents.y_bearing),
            (extents.x_bearing + extents.width, extents.y_bearing),
            (extents.x_bearing, extents.y_bearing - extents.height),
            (
                extents.x_bearing + extents.width,
                extents.y_bearing - extents.height,
            ),
        ];
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (x, y) in corners {
            let (x, y) = self.to_px.transform(x, y);
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
        if !(min_x.is_finite() && min_y.is_finite() && max_x.is_finite() && max_y.is_finite()) {
            return false;
        }
        let (x0, y0) = (min_x.floor(), min_y.floor());
        let (x1, y1) = (max_x.ceil(), max_y.ceil());
        if x1 - x0 > MAX_PIXELS as f32 || y1 - y0 > MAX_PIXELS as f32 {
            return false;
        }
        let (width, height) = ((x1 - x0) as usize, (y1 - y0) as usize);
        if width.saturating_mul(height) > MAX_PIXELS {
            return false;
        }
        self.x0 = x0 as i32;
        self.y0 = y0 as i32;
        self.width = width;
        self.height = height;

        let n = width * height;
        let scratch = &mut *self.buffers.scratch;
        scratch.layers.clear();
        scratch.layers.resize(n * 4, 0);
        scratch.frames.clear();
        scratch.clips.clear();
        scratch.clip_count = 0;
        scratch.transforms.clear();
        scratch.transforms.push(Matrix::IDENTITY);

        let ok = glyph.paint(&mut self).is_ok();
        while !self.buffers.scratch.frames.is_empty() {
            self.pop_layer();
        }
        if !ok {
            return false;
        }
        image.data.clear();
        image
            .data
            .extend_from_slice(&self.buffers.scratch.layers[..n * 4]);
        image.placement = Placement {
            left: self.x0,
            top: self.y0 + height as i32,
            width: width as u32,
            height: height as u32,
        };
        true
    }

    fn ctm(&self) -> Matrix<f32> {
        *self
            .buffers
            .scratch
            .transforms
            .last()
            .unwrap_or(&Matrix::IDENTITY)
    }

    fn pixels(&self) -> usize {
        self.width * self.height
    }

    /// Rasterizes `path`, already in pixels, into the coverage buffer.
    fn rasterize(&mut self, path: impl PathData) {
        let scratch = &mut *self.buffers.scratch;
        scratch.coverage.clear();
        scratch.coverage.resize(self.width * self.height, 0);
        if self.width == 0 || self.height == 0 {
            return;
        }
        Mask::with_scratch(path, self.buffers.rcx)
            .size(self.width as u32, self.height as u32)
            .origin(Origin::BottomLeft)
            .offset(Vector::new(-(self.x0 as f32), -(self.y0 as f32)))
            .render_into(&mut scratch.coverage[..], None);
    }

    /// Rasterizes the outline of `glyph` under the current transform.
    fn rasterize_glyph(&mut self, glyph: GlyphId) {
        let outline = &mut *self.buffers.outline;
        outline.clear();
        if let Some(scaled) = self.scaler.outline(glyph) {
            let needed = scaled.required_memory_size();
            if self.buffers.draw_memory.len() < needed {
                self.buffers.draw_memory.resize(needed, 0);
            }
            outline.begin_layer(None);
            let memory = DrawMemory::Buffer(&mut self.buffers.draw_memory[..]);
            if scaled.draw(memory, outline).is_err() {
                outline.clear();
            }
            outline.maybe_close();
            outline.finish();
        }
        // The scaler's points are already scaled, so undo that before the
        // font-unit transform and redo it on the way out.
        let m = self.to_px * self.ctm() * scaled(1. / self.scale);
        let outline = &mut *self.buffers.outline;
        outline.transform(&to_zeno(m));
        let scratch = &mut *self.buffers.scratch;
        scratch.coverage.clear();
        scratch.coverage.resize(self.width * self.height, 0);
        if self.width == 0 || self.height == 0 {
            return;
        }
        Mask::with_scratch(outline.path(), self.buffers.rcx)
            .size(self.width as u32, self.height as u32)
            .origin(Origin::BottomLeft)
            .offset(Vector::new(-(self.x0 as f32), -(self.y0 as f32)))
            .render_into(&mut scratch.coverage[..], None);
    }

    /// Pushes the coverage buffer, intersected with the current clip.
    fn push_clip(&mut self) {
        let n = self.pixels();
        let scratch = &mut *self.buffers.scratch;
        let depth = scratch.clip_count;
        scratch.clips.resize((depth + 1) * n, 0);
        let (below, top) = scratch.clips.split_at_mut(depth * n);
        if depth == 0 {
            top.copy_from_slice(&scratch.coverage[..n]);
        } else {
            let prev = &below[(depth - 1) * n..];
            for ((t, &p), &c) in top.iter_mut().zip(prev).zip(&scratch.coverage) {
                *t = mul255(p, c);
            }
        }
        scratch.clip_count = depth + 1;
    }

    /// Fills the top layer through the current clip, and through the
    /// coverage buffer as well when `with_coverage` is set.
    fn fill_masked(&mut self, brush: Brush<'_>, with_coverage: bool) {
        let n = self.pixels();
        if n == 0 {
            return;
        }
        let mut stops = core::mem::take(&mut self.buffers.scratch.stops);
        let shader = self.shader(brush, &mut stops);
        if let Some(shader) = shader {
            let scratch = &mut *self.buffers.scratch;
            let clip = (scratch.clip_count > 0)
                .then(|| &scratch.clips[(scratch.clip_count - 1) * n..][..n]);
            let coverage = with_coverage.then(|| &scratch.coverage[..n]);
            let len = scratch.layers.len();
            let layer = &mut scratch.layers[len - n * 4..];
            let (width, x0) = (self.width, self.x0 as f32);
            let top = (self.y0 + self.height as i32) as f32;
            for (y, row) in layer.chunks_exact_mut(width * 4).enumerate() {
                let py = top - y as f32 - 0.5;
                for (x, px) in row.chunks_exact_mut(4).enumerate() {
                    let i = y * width + x;
                    let mut c = clip.map_or(255, |clip| clip[i]);
                    if let Some(coverage) = coverage {
                        c = mul255(c, coverage[i]);
                    }
                    if c == 0 {
                        continue;
                    }
                    let Some(mut color) = shader.at(x0 + x as f32 + 0.5, py) else {
                        continue;
                    };
                    if c != 255 {
                        let k = c as f32 / 255.;
                        for v in &mut color {
                            *v *= k;
                        }
                    }
                    let dst = unpack(px);
                    pack(px, src_over(color, dst));
                }
            }
        }
        stops.clear();
        self.buffers.scratch.stops = stops;
    }

    fn color(&self, palette_index: u16, alpha: f32) -> [f32; 4] {
        let rgba = if palette_index == 0xFFFF {
            self.foreground
        } else {
            self.palette
                .as_ref()
                .map_or([0; 4], |p| p.color(palette_index))
        };
        let mut color = rgba.map(|v| v as f32 / 255.);
        color[3] *= alpha.clamp(0., 1.);
        color
    }

    fn resolve_stops(&self, color_stops: &[ColorStop], stops: &mut Vec<Stop>) {
        stops.clear();
        stops.extend(color_stops.iter().map(|stop| Stop {
            offset: stop.offset,
            color: self.color(stop.palette_index, stop.alpha),
        }));
    }

    fn shader<'s>(&self, brush: Brush<'_>, stops: &'s mut Vec<Stop>) -> Option<Shader<'s>> {
        let inverse = invert(self.to_px * self.ctm())?;
        Some(match brush {
            Brush::Solid {
                palette_index,
                alpha,
            } => Shader::Solid(premultiply(self.color(palette_index, alpha))),
            Brush::LinearGradient {
                p0,
                p1,
                color_stops,
                extend,
            } => {
                self.resolve_stops(color_stops, stops);
                let d = p1 - p0;
                let dd = d.x * d.x + d.y * d.y;
                if dd <= 0. {
                    return None;
                }
                Shader::Gradient {
                    inverse,
                    kind: Kind::Linear {
                        p0,
                        d,
                        inv_dd: 1. / dd,
                    },
                    extend,
                    stops,
                }
            }
            Brush::RadialGradient {
                c0,
                r0,
                c1,
                r1,
                color_stops,
                extend,
            } => {
                self.resolve_stops(color_stops, stops);
                Shader::Gradient {
                    inverse,
                    kind: Kind::Radial {
                        c0,
                        r0,
                        cd: c1 - c0,
                        dr: r1 - r0,
                    },
                    extend,
                    stops,
                }
            }
            Brush::SweepGradient {
                c0,
                start_angle,
                end_angle,
                color_stops,
                extend,
            } => {
                self.resolve_stops(color_stops, stops);
                let sweep = end_angle - start_angle;
                if sweep == 0. {
                    return None;
                }
                Shader::Gradient {
                    inverse,
                    kind: Kind::Sweep {
                        c0,
                        start: start_angle,
                        inv_sweep: 1. / sweep,
                    },
                    extend,
                    stops,
                }
            }
        })
    }
}

impl PaintSink for Painter<'_> {
    fn push_transform(&mut self, transform: Matrix<f32>) {
        let m = self.ctm() * transform;
        self.buffers.scratch.transforms.push(m);
    }

    fn pop_transform(&mut self) {
        let transforms = &mut self.buffers.scratch.transforms;
        if transforms.len() > 1 {
            transforms.pop();
        }
    }

    fn push_clip_glyph(&mut self, glyph: GlyphId) {
        self.rasterize_glyph(glyph);
        self.push_clip();
    }

    fn push_clip_box(&mut self, clip_box: BoundingBox<f32>) {
        let m = self.to_px * self.ctm();
        let corner = |x, y| {
            let (x, y) = m.transform(x, y);
            Vector::new(x, y)
        };
        let path = [
            Command::MoveTo(corner(clip_box.x_min, clip_box.y_min)),
            Command::LineTo(corner(clip_box.x_max, clip_box.y_min)),
            Command::LineTo(corner(clip_box.x_max, clip_box.y_max)),
            Command::LineTo(corner(clip_box.x_min, clip_box.y_max)),
            Command::Close,
        ];
        self.rasterize(&path[..]);
        self.push_clip();
    }

    fn pop_clip(&mut self) {
        let n = self.pixels();
        let scratch = &mut *self.buffers.scratch;
        if scratch.clip_count > 0 {
            scratch.clip_count -= 1;
            scratch.clips.truncate(scratch.clip_count * n);
        }
    }

    fn fill(&mut self, brush: Brush<'_>) {
        self.fill_masked(brush, false);
    }

    fn fill_glyph(
        &mut self,
        glyph: GlyphId,
        brush_transform: Option<Matrix<f32>>,
        brush: Brush<'_>,
    ) {
        self.rasterize_glyph(glyph);
        if let Some(transform) = brush_transform {
            self.push_transform(transform);
            self.fill_masked(brush, true);
            self.pop_transform();
        } else {
            self.fill_masked(brush, true);
        }
    }

    fn push_layer(&mut self, composite_mode: CompositeMode) {
        let n = self.pixels();
        let scratch = &mut *self.buffers.scratch;
        scratch.frames.push(Frame {
            mode: composite_mode,
            clip: scratch.clip_count,
        });
        let len = scratch.layers.len();
        scratch.layers.resize(len + n * 4, 0);
    }

    fn pop_layer(&mut self) {
        let n = self.pixels();
        let scratch = &mut *self.buffers.scratch;
        let Some(frame) = scratch.frames.pop() else {
            return;
        };
        let len = scratch.layers.len();
        let (below, top) = scratch.layers.split_at_mut(len - n * 4);
        let below = &mut below[len - 2 * n * 4..];
        let depth = frame.clip.min(scratch.clip_count);
        let clip = (depth > 0).then(|| &scratch.clips[(depth - 1) * n..][..n]);
        for (i, (dst, src)) in below
            .chunks_exact_mut(4)
            .zip(top.chunks_exact(4))
            .enumerate()
        {
            let c = clip.map_or(255, |clip| clip[i]);
            if c == 0 {
                continue;
            }
            let (s, d) = (unpack(src), unpack(dst));
            let mut out = composite(frame.mode, s, d);
            if c != 255 {
                let k = c as f32 / 255.;
                for (o, d) in out.iter_mut().zip(d) {
                    *o = d + (*o - d) * k;
                }
            }
            pack(dst, out);
        }
        scratch.layers.truncate(len - n * 4);
    }
}

enum Shader<'s> {
    /// Premultiplied.
    Solid([f32; 4]),
    Gradient {
        /// Pixels to the gradient's font-unit space.
        inverse: Matrix<f32>,
        kind: Kind,
        extend: Extend,
        stops: &'s [Stop],
    },
}

enum Kind {
    Linear {
        p0: Point<f32>,
        d: Point<f32>,
        inv_dd: f32,
    },
    Radial {
        c0: Point<f32>,
        r0: f32,
        cd: Point<f32>,
        dr: f32,
    },
    Sweep {
        c0: Point<f32>,
        /// Degrees clockwise, as read-fonts normalizes them.
        start: f32,
        inv_sweep: f32,
    },
}

impl Shader<'_> {
    /// Premultiplied color at a pixel center, or `None` where the brush
    /// paints nothing.
    fn at(&self, x: f32, y: f32) -> Option<[f32; 4]> {
        match self {
            Shader::Solid(color) => Some(*color),
            Shader::Gradient {
                inverse,
                kind,
                extend,
                stops,
            } => {
                let (x, y) = inverse.transform(x, y);
                let t = match *kind {
                    Kind::Linear { p0, d, inv_dd } => {
                        ((x - p0.x) * d.x + (y - p0.y) * d.y) * inv_dd
                    }
                    Kind::Radial { c0, r0, cd, dr } => radial_t(x - c0.x, y - c0.y, r0, cd, dr)?,
                    Kind::Sweep {
                        c0,
                        start,
                        inv_sweep,
                    } => {
                        // Counter-clockwise from +x in the font's y-up space,
                        // then into the clockwise range read-fonts uses.
                        let mut degrees = (y - c0.y).atan2(x - c0.x).to_degrees();
                        if degrees < 0. {
                            degrees += 360.;
                        }
                        let mut clockwise = 360. - degrees;
                        if clockwise >= 360. {
                            clockwise -= 360.;
                        }
                        (clockwise - start) * inv_sweep
                    }
                };
                if !t.is_finite() {
                    return None;
                }
                let t = match extend {
                    Extend::Repeat => t - t.floor(),
                    Extend::Reflect => {
                        let m = t.rem_euclid(2.);
                        if m > 1. {
                            2. - m
                        } else {
                            m
                        }
                    }
                    _ => t.clamp(0., 1.),
                };
                Some(premultiply(color_at(stops, t)))
            }
        }
    }
}

/// Largest `t` whose circle, interpolated between the two of a radial
/// gradient, passes through the point and has a non-negative radius.
fn radial_t(px: f32, py: f32, r0: f32, cd: Point<f32>, dr: f32) -> Option<f32> {
    let a = cd.x * cd.x + cd.y * cd.y - dr * dr;
    let b = px * cd.x + py * cd.y + r0 * dr;
    let c = px * px + py * py - r0 * r0;
    if a.abs() < 1e-6 {
        if b.abs() < 1e-12 {
            return None;
        }
        let t = c / (2. * b);
        return (r0 + t * dr >= 0.).then_some(t);
    }
    let disc = b * b - a * c;
    if disc < 0. {
        return None;
    }
    let root = disc.sqrt();
    let (t1, t2) = ((b + root) / a, (b - root) / a);
    let (hi, lo) = if t1 > t2 { (t1, t2) } else { (t2, t1) };
    if r0 + hi * dr >= 0. {
        Some(hi)
    } else if r0 + lo * dr >= 0. {
        Some(lo)
    } else {
        None
    }
}

/// Straight color at `t` along stops sorted by offset; among stops at one
/// offset the last one wins.
fn color_at(stops: &[Stop], t: f32) -> [f32; 4] {
    let (Some(first), Some(last)) = (stops.first(), stops.last()) else {
        return [0.; 4];
    };
    if t <= first.offset {
        return first.color;
    }
    if t >= last.offset {
        return last.color;
    }
    for pair in stops.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if t >= a.offset && t < b.offset {
            let f = (t - a.offset) / (b.offset - a.offset);
            let mut color = [0.; 4];
            for (c, (x, y)) in color.iter_mut().zip(a.color.iter().zip(&b.color)) {
                *c = x + (y - x) * f;
            }
            return color;
        }
    }
    last.color
}

fn premultiply(c: [f32; 4]) -> [f32; 4] {
    [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]]
}

fn unpack(px: &[u8]) -> [f32; 4] {
    [
        px[0] as f32 / 255.,
        px[1] as f32 / 255.,
        px[2] as f32 / 255.,
        px[3] as f32 / 255.,
    ]
}

fn pack(px: &mut [u8], c: [f32; 4]) {
    for (p, v) in px.iter_mut().zip(c) {
        *p = (v.clamp(0., 1.) * 255. + 0.5) as u8;
    }
}

fn mul255(a: u8, b: u8) -> u8 {
    ((a as u32 * b as u32 + 127) / 255) as u8
}

fn src_over(s: [f32; 4], d: [f32; 4]) -> [f32; 4] {
    let k = 1. - s[3];
    [
        s[0] + d[0] * k,
        s[1] + d[1] * k,
        s[2] + d[2] * k,
        s[3] + d[3] * k,
    ]
}

/// Composites premultiplied `s` onto `d` (Porter-Duff and the W3C blend
/// modes), returning a premultiplied color.
fn composite(mode: CompositeMode, s: [f32; 4], d: [f32; 4]) -> [f32; 4] {
    let (sa, da) = (s[3], d[3]);
    let scale = |c: [f32; 4], k: f32| [c[0] * k, c[1] * k, c[2] * k, c[3] * k];
    let add = |a: [f32; 4], b: [f32; 4]| [a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3]];
    match mode {
        CompositeMode::Clear => [0.; 4],
        CompositeMode::Src => s,
        CompositeMode::Dest => d,
        CompositeMode::SrcOver | CompositeMode::Unknown => src_over(s, d),
        CompositeMode::DestOver => src_over(d, s),
        CompositeMode::SrcIn => scale(s, da),
        CompositeMode::DestIn => scale(d, sa),
        CompositeMode::SrcOut => scale(s, 1. - da),
        CompositeMode::DestOut => scale(d, 1. - sa),
        CompositeMode::SrcAtop => add(scale(s, da), scale(d, 1. - sa)),
        CompositeMode::DestAtop => add(scale(d, sa), scale(s, 1. - da)),
        CompositeMode::Xor => add(scale(s, 1. - da), scale(d, 1. - sa)),
        CompositeMode::Plus => add(s, d).map(|v| v.min(1.)),
        _ => {
            let cs = if sa > 0. {
                [s[0] / sa, s[1] / sa, s[2] / sa]
            } else {
                [0.; 3]
            };
            let cb = if da > 0. {
                [d[0] / da, d[1] / da, d[2] / da]
            } else {
                [0.; 3]
            };
            let b = match mode {
                CompositeMode::HslHue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
                CompositeMode::HslSaturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
                CompositeMode::HslColor => set_lum(cs, lum(cb)),
                CompositeMode::HslLuminosity => set_lum(cb, lum(cs)),
                _ => [
                    blend(mode, cb[0], cs[0]),
                    blend(mode, cb[1], cs[1]),
                    blend(mode, cb[2], cs[2]),
                ],
            };
            let mut out = [0.; 4];
            for c in 0..3 {
                out[c] = (1. - da) * s[c] + (1. - sa) * d[c] + sa * da * b[c];
            }
            out[3] = sa + da * (1. - sa);
            out
        }
    }
}

/// Separable blend of backdrop `cb` with source `cs`, both straight.
fn blend(mode: CompositeMode, cb: f32, cs: f32) -> f32 {
    let multiply = |cb: f32, cs: f32| cb * cs;
    let screen = |cb: f32, cs: f32| cb + cs - cb * cs;
    let hard_light = |cb: f32, cs: f32| {
        if cs <= 0.5 {
            multiply(cb, 2. * cs)
        } else {
            screen(cb, 2. * cs - 1.)
        }
    };
    match mode {
        CompositeMode::Screen => screen(cb, cs),
        CompositeMode::Overlay => hard_light(cs, cb),
        CompositeMode::Darken => cb.min(cs),
        CompositeMode::Lighten => cb.max(cs),
        CompositeMode::ColorDodge => {
            if cb <= 0. {
                0.
            } else if cs >= 1. {
                1.
            } else {
                (cb / (1. - cs)).min(1.)
            }
        }
        CompositeMode::ColorBurn => {
            if cb >= 1. {
                1.
            } else if cs <= 0. {
                0.
            } else {
                1. - ((1. - cb) / cs).min(1.)
            }
        }
        CompositeMode::HardLight => hard_light(cb, cs),
        CompositeMode::SoftLight => {
            if cs <= 0.5 {
                cb - (1. - 2. * cs) * cb * (1. - cb)
            } else {
                let d = if cb <= 0.25 {
                    ((16. * cb - 12.) * cb + 4.) * cb
                } else {
                    cb.sqrt()
                };
                cb + (2. * cs - 1.) * (d - cb)
            }
        }
        CompositeMode::Difference => (cb - cs).abs(),
        CompositeMode::Exclusion => cb + cs - 2. * cb * cs,
        _ => multiply(cb, cs),
    }
}

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut out = c;
    if n < 0. {
        for v in &mut out {
            *v = l + (*v - l) * l / (l - n);
        }
    }
    if x > 1. {
        for v in &mut out {
            *v = l + (*v - l) * (1. - l) / (x - l);
        }
    }
    out
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mut idx = [0, 1, 2];
    idx.sort_by(|&a, &b| {
        c[a].partial_cmp(&c[b])
            .unwrap_or(core::cmp::Ordering::Equal)
    });
    let (min, mid, max) = (idx[0], idx[1], idx[2]);
    let mut out = [0.; 3];
    if c[max] > c[min] {
        out[mid] = (c[mid] - c[min]) * s / (c[max] - c[min]);
        out[max] = s;
    }
    out
}

fn scaled(s: f32) -> Matrix<f32> {
    Matrix {
        xx: s,
        yy: s,
        ..Matrix::IDENTITY
    }
}

/// zeno's `Transform` into read-fonts' `Matrix` (their `xy`/`yx` names are
/// swapped).
fn from_zeno(t: Transform) -> Matrix<f32> {
    Matrix {
        xx: t.xx,
        xy: t.yx,
        yx: t.xy,
        yy: t.yy,
        dx: t.x,
        dy: t.y,
    }
}

fn to_zeno(m: Matrix<f32>) -> Transform {
    Transform::new(m.xx, m.yx, m.xy, m.yy, m.dx, m.dy)
}

fn invert(m: Matrix<f32>) -> Option<Matrix<f32>> {
    let det = m.xx * m.yy - m.xy * m.yx;
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let (xx, xy, yx, yy) = (m.yy / det, -m.xy / det, -m.yx / det, m.xx / det);
    Some(Matrix {
        xx,
        xy,
        yx,
        yy,
        dx: -(xx * m.dx + xy * m.dy),
        dy: -(yx * m.dx + yy * m.dy),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-4)
    }

    #[test]
    fn porter_duff_identities() {
        let s = [0.2, 0.0, 0.0, 0.5];
        let d = [0.0, 0.0, 0.8, 0.8];
        assert!(close(
            composite(CompositeMode::SrcOver, s, d),
            [0.2, 0., 0.4, 0.9]
        ));
        assert!(close(
            composite(CompositeMode::DestOver, s, d),
            [0.04, 0., 0.8, 0.9]
        ));
        assert!(close(
            composite(CompositeMode::SrcIn, s, d),
            [0.16, 0., 0., 0.4]
        ));
        assert!(close(
            composite(CompositeMode::DestOut, s, d),
            [0., 0., 0.4, 0.4]
        ));
        assert!(close(
            composite(CompositeMode::Xor, s, d),
            [0.04, 0., 0.4, 0.5]
        ));
        assert!(close(composite(CompositeMode::Clear, s, d), [0.; 4]));
    }

    #[test]
    fn blend_modes_against_opaque_backdrop() {
        let s = [0.5, 0.5, 0.5, 1.];
        let d = [0.25, 0.75, 1., 1.];
        assert!(close(
            composite(CompositeMode::Multiply, s, d),
            [0.125, 0.375, 0.5, 1.]
        ));
        assert!(close(
            composite(CompositeMode::Screen, s, d),
            [0.625, 0.875, 1., 1.]
        ));
        assert!(close(
            composite(CompositeMode::Difference, s, d),
            [0.25, 0.25, 0.5, 1.]
        ));
        assert!(close(
            composite(CompositeMode::Darken, s, d),
            [0.25, 0.5, 0.5, 1.]
        ));
        // Luminosity of a gray source onto a colored backdrop keeps the hue.
        let out = composite(CompositeMode::HslLuminosity, s, d);
        assert!((lum([out[0], out[1], out[2]]) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn stop_lookup_uses_the_last_of_coincident_stops() {
        let stops = [
            Stop {
                offset: 0.,
                color: [1., 0., 0., 1.],
            },
            Stop {
                offset: 0.5,
                color: [0., 1., 0., 1.],
            },
            Stop {
                offset: 0.5,
                color: [0., 0., 1., 1.],
            },
            Stop {
                offset: 1.,
                color: [0., 0., 0., 1.],
            },
        ];
        assert_eq!(color_at(&stops, 0.25), [0.5, 0.5, 0., 1.]);
        assert_eq!(color_at(&stops, 0.5), [0., 0., 1., 1.]);
        assert_eq!(color_at(&stops, 0.75), [0., 0., 0.5, 1.]);
        assert_eq!(color_at(&stops, 2.), [0., 0., 0., 1.]);
    }

    #[test]
    fn radial_picks_the_larger_circle() {
        // Concentric circles r0 = 0, r1 = 10 at the origin.
        let t = radial_t(5., 0., 0., Point::new(0., 0.), 10.).unwrap();
        assert!((t - 0.5).abs() < 1e-5);
        // Shrinking concentric circles reach a point outside the start circle
        // at a negative t, which the extend mode then resolves.
        let t = radial_t(20., 0., 10., Point::new(0., 0.), -10.).unwrap();
        assert!((t + 1.).abs() < 1e-5);
        // A cone opening away from the point never covers it.
        assert!(radial_t(-10., 0., 0., Point::new(10., 0.), 5.).is_none());
    }

    #[test]
    fn matrix_round_trips_through_zeno_and_inverts() {
        let m = Matrix {
            xx: 2.,
            xy: 0.5,
            yx: -1.,
            yy: 3.,
            dx: 7.,
            dy: -4.,
        };
        let z = to_zeno(m);
        let p = z.transform_point(Vector::new(1., 2.));
        let (x, y) = m.transform(1., 2.);
        assert_eq!((p.x, p.y), (x, y));
        assert_eq!(from_zeno(z), m);
        let inv = invert(m).unwrap();
        let (bx, by) = inv.transform(x, y);
        assert!((bx - 1.).abs() < 1e-5 && (by - 2.).abs() < 1e-5);
    }
}
