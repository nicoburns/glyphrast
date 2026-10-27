//! Byte-for-byte comparison against swash 0.2.10 built on the same skrifa.

use glyphrast::image::Image;
use glyphrast::zeno::{Angle, Format, Transform, Vector};
use glyphrast::{Font, FontKey, RasterContext, Render, Source, StrikeWith};
use std::sync::Arc;

const SYSTEM_FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
    "/usr/share/texmf/fonts/opentype/public/lm/lmroman10-regular.otf",
];

fn test_font(name: &str) -> String {
    format!("{}/tests/fonts/{name}", env!("CARGO_MANIFEST_DIR"))
}

struct Fixture {
    name: String,
    data: Arc<Vec<u8>>,
    font: Font,
    num_glyphs: u32,
}

fn load(path: &str) -> Option<Fixture> {
    let data = Arc::new(std::fs::read(path).ok()?);
    let font = Font::new(data.clone() as Arc<dyn AsRef<[u8]> + Send + Sync>, 0)?;
    let num_glyphs = font.num_glyphs();
    Some(Fixture {
        name: path.rsplit('/').next().unwrap().to_string(),
        data,
        font,
        num_glyphs,
    })
}

fn swash_font(f: &Fixture) -> swash::FontRef<'_> {
    swash::FontRef::from_index(&f.data, 0).unwrap()
}

#[derive(Default, Debug)]
struct Report {
    compared: usize,
    mismatched: usize,
    examples: Vec<String>,
}

impl Report {
    fn check(&mut self, label: impl FnOnce() -> String, same: bool) {
        self.compared += 1;
        if !same {
            self.mismatched += 1;
            if self.examples.len() < 12 {
                self.examples.push(label());
            }
        }
    }
    fn finish(self, what: &str) {
        println!(
            "{what}: {} compared, {} mismatched",
            self.compared, self.mismatched
        );
        for e in &self.examples {
            println!("  {e}");
        }
        assert_eq!(self.mismatched, 0, "{what} differs from swash");
        assert!(self.compared > 0, "{what}: nothing compared");
    }
}

fn same_image(a: &Option<swash::scale::image::Image>, b: &Option<Image>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            format!("{:?}", a.placement) == format!("{:?}", b.placement)
                && a.data == b.data
                && format!("{:?}", a.content) == format!("{:?}", b.content)
        }
        _ => false,
    }
}

fn describe(a: &Option<swash::scale::image::Image>, b: &Option<Image>) -> String {
    match (a, b) {
        (Some(a), Some(b)) => {
            let diff = a
                .data
                .iter()
                .zip(b.data.iter())
                .filter(|(x, y)| x != y)
                .count();
            format!(
                "swash {:?} {:?} {}B vs new {:?} {:?} {}B, {} bytes differ",
                a.placement,
                a.content,
                a.data.len(),
                b.placement,
                b.content,
                b.data.len(),
                diff
            )
        }
        (a, b) => format!("swash {} vs new {}", a.is_some(), b.is_some()),
    }
}

fn glyph_ids(num_glyphs: u32, max: u32) -> impl Iterator<Item = u32> {
    let step = (num_glyphs / max).max(1);
    (0..num_glyphs).step_by(step as usize)
}

const SIZES: &[f32] = &[0., 8.3, 12., 16., 24.5, 48.];

#[test]
fn outlines_match_swash() {
    let mut report = Report::default();
    let mut raster = Report::default();
    let mut paths: Vec<String> = SYSTEM_FONTS.iter().map(|s| s.to_string()).collect();
    for name in [
        "tthint_subset.ttf",
        "cousine_hint_subset.ttf",
        "tinos_subset.ttf",
        "NotoSansJP-Regular.subset.otf",
        "noto_serif_display_trimmed.ttf",
        "material_icons_subset.ttf",
        "vazirmatn_var_trimmed.ttf",
        "cantarell_vf_trimmed.ttf",
        "colrv0_dejavu.ttf",
    ] {
        paths.push(test_font(name));
    }
    let mut new_cx = RasterContext::new();
    let mut old_cx = swash::scale::ScaleContext::new();
    for (key, path) in paths.iter().enumerate() {
        let Some(f) = load(path) else {
            println!("skipping missing {path}");
            continue;
        };
        let sf = swash_font(&f);
        let coord_sets: Vec<Vec<i16>> = if f.font.axes().is_empty() {
            vec![vec![]]
        } else {
            vec![
                vec![],
                vec![-16384; 1],
                vec![8192, -4096],
                vec![16383, 16383, 16383],
            ]
        };
        for &size in SIZES {
            for hint in [false, true] {
                for coords in &coord_sets {
                    let mut old = old_cx
                        .builder(sf)
                        .size(size)
                        .hint(hint)
                        .normalized_coords(coords.iter().copied())
                        .build();
                    let mut new = new_cx
                        .builder(&f.font, FontKey(key as u64))
                        .size(size)
                        .hint(hint)
                        .normalized_coords(coords.iter().copied())
                        .build();
                    assert_eq!(old.has_outlines(), new.has_outlines(), "{}", f.name);
                    assert_eq!(
                        old.has_color_outlines(),
                        new.has_color_outlines(),
                        "{}",
                        f.name
                    );
                    for gid in glyph_ids(f.num_glyphs, 60) {
                        let a = old.scale_outline(gid as u16);
                        let b = new.scale_outline(gid);
                        let same = match (&a, &b) {
                            (None, None) => true,
                            (Some(a), Some(b)) => {
                                a.points() == b.points()
                                    && a.verbs() == b.verbs()
                                    && a.len() == b.len()
                                    && a.is_color() == b.is_color()
                            }
                            _ => false,
                        };
                        report.check(
                            || format!("{} gid {gid} size {size} hint {hint} coords {coords:?}: swash {} new {}", f.name, a.is_some(), b.is_some()),
                            same,
                        );
                        if size == 0. {
                            continue;
                        }
                        let variants: [(Format, Vector, f32, Option<Transform>); 5] = [
                            (Format::Alpha, Vector::ZERO, 0., None),
                            (Format::Alpha, Vector::new(0.3, 0.6), 0., None),
                            (Format::Subpixel, Vector::new(0.125, 0.), 0., None),
                            (Format::Alpha, Vector::ZERO, 1.0, None),
                            (
                                Format::Alpha,
                                Vector::new(0.5, 0.25),
                                0.,
                                Some(Transform::skew(
                                    Angle::from_degrees(14.),
                                    Angle::from_degrees(0.),
                                )),
                            ),
                        ];
                        // swash computes the embolden orientation over all contours as one
                        // polygon, which is wrong for glyphs with several contours; only
                        // single-contour glyphs are expected to match when emboldening.
                        let contours = b
                            .as_ref()
                            .map(|o| {
                                o.verbs()
                                    .iter()
                                    .filter(|v| **v == glyphrast::zeno::Verb::MoveTo)
                                    .count()
                            })
                            .unwrap_or(0);
                        for (format, offset, embolden, transform) in variants {
                            if embolden != 0. && contours > 1 {
                                continue;
                            }
                            let a = swash::scale::Render::new(&[swash::scale::Source::Outline])
                                .format(format)
                                .offset(offset)
                                .embolden(embolden)
                                .transform(transform)
                                .render(&mut old, gid as u16);
                            let b = Render::new(&[Source::Outline])
                                .format(format)
                                .offset(offset)
                                .embolden(embolden)
                                .transform(transform)
                                .render(&mut new, gid);
                            raster.check(
                                || format!("{} gid {gid} size {size} hint {hint} coords {coords:?} {format:?} {offset:?} e{embolden} t{}: {}", f.name, transform.is_some(), describe(&a, &b)),
                                same_image(&a, &b),
                            );
                        }
                    }
                }
            }
        }
    }
    report.finish("scale_outline");
    raster.finish("Render outline");
}

#[test]
fn color_outlines_match_swash() {
    let mut report = Report::default();
    let f = load(&test_font("colrv0_dejavu.ttf")).unwrap();
    let sf = swash_font(&f);
    let mut new_cx = RasterContext::new();
    let mut old_cx = swash::scale::ScaleContext::new();
    for &size in SIZES {
        for hint in [false, true] {
            let mut old = old_cx.builder(sf).size(size).hint(hint).build();
            let mut new = new_cx
                .builder(&f.font, FontKey(7))
                .size(size)
                .hint(hint)
                .build();
            assert!(new.has_color_outlines());
            for gid in 0..f.num_glyphs {
                let a = old.scale_color_outline(gid as u16);
                let b = new.scale_color_outline(gid);
                let same = match (&a, &b) {
                    (None, None) => true,
                    (Some(a), Some(b)) => {
                        a.points() == b.points()
                            && a.verbs() == b.verbs()
                            && a.len() == b.len()
                            && a.is_color() == b.is_color()
                            && (0..a.len()).all(|i| {
                                a.get(i).unwrap().color_index() == b.get(i).unwrap().color_index()
                            })
                    }
                    _ => false,
                };
                report.check(
                    || {
                        format!(
                            "color outline gid {gid} size {size} hint {hint}: swash {} new {}",
                            a.is_some(),
                            b.is_some()
                        )
                    },
                    same,
                );
                if size == 0. {
                    continue;
                }
                for palette in [0u16, 1, 5] {
                    for offset in [Vector::ZERO, Vector::new(0.4, 0.7)] {
                        let sources = [
                            swash::scale::Source::ColorOutline(palette),
                            swash::scale::Source::Outline,
                        ];
                        let a = swash::scale::Render::new(&sources)
                            .offset(offset)
                            .default_color([10, 20, 30, 200])
                            .render(&mut old, gid as u16);
                        let b = Render::new(&[Source::ColorOutline(palette), Source::Outline])
                            .offset(offset)
                            .default_color([10, 20, 30, 200])
                            .render(&mut new, gid);
                        let same = same_image(&a, &b)
                            && match (&a, &b) {
                                (Some(a), Some(b)) => {
                                    format!("{:?}", a.source) == format!("{:?}", b.source)
                                }
                                _ => true,
                            };
                        report.check(|| format!("Render color gid {gid} size {size} hint {hint} palette {palette} {offset:?}: {}", describe(&a, &b)), same);
                    }
                }
            }
        }
    }
    report.finish("COLRv0");
}

#[test]
fn bitmaps_match_swash() {
    let mut report = Report::default();
    let mut paths = vec![
        test_font("cbdt.ttf"),
        test_font("embedded_bitmaps.ttf"),
        test_font("noto_handwriting-sbix.ttf"),
    ];
    if let Ok(p) = std::env::var("NOTO_COLOR_EMOJI") {
        paths.push(p);
    }
    let mut new_cx = RasterContext::new();
    let mut old_cx = swash::scale::ScaleContext::new();
    for (key, path) in paths.iter().enumerate() {
        let f = load(path).unwrap();
        let sf = swash_font(&f);
        for &size in &[0., 9., 20., 32., 109., 128., 136., 200.] {
            let mut old = old_cx.builder(sf).size(size).build();
            let mut new = new_cx
                .builder(&f.font, FontKey(100 + key as u64))
                .size(size)
                .build();
            assert_eq!(old.has_bitmaps(), new.has_bitmaps(), "{}", f.name);
            assert_eq!(
                old.has_color_bitmaps(),
                new.has_color_bitmaps(),
                "{}",
                f.name
            );
            for gid in glyph_ids(f.num_glyphs, 80) {
                // cbdt.ttf glyph 1 has a zero-length image record. swash decodes the
                // next glyph's image for it; read-fonts reports no bitmap.
                if f.name == "cbdt.ttf" && gid == 1 {
                    continue;
                }
                for (os, ns) in [
                    (swash::scale::StrikeWith::ExactSize, StrikeWith::ExactSize),
                    (swash::scale::StrikeWith::BestFit, StrikeWith::BestFit),
                    (
                        swash::scale::StrikeWith::LargestSize,
                        StrikeWith::LargestSize,
                    ),
                    (swash::scale::StrikeWith::Index(0), StrikeWith::Index(0)),
                    (swash::scale::StrikeWith::Index(2), StrikeWith::Index(2)),
                ] {
                    for color in [false, true] {
                        let a = if color {
                            old.scale_color_bitmap(gid as u16, os)
                        } else {
                            old.scale_bitmap(gid as u16, os)
                        };
                        let b = if color {
                            new.scale_color_bitmap(gid, ns)
                        } else {
                            new.scale_bitmap(gid, ns)
                        };
                        report.check(
                            || {
                                format!(
                                    "{} gid {gid} size {size} {ns:?} color {color}: {}",
                                    f.name,
                                    describe(&a, &b)
                                )
                            },
                            same_image(&a, &b),
                        );
                    }
                    let a = swash::scale::Render::new(&[
                        swash::scale::Source::ColorBitmap(os),
                        swash::scale::Source::Bitmap(os),
                        swash::scale::Source::Outline,
                    ])
                    .render(&mut old, gid as u16);
                    let b = Render::new(&[
                        Source::ColorBitmap(ns),
                        Source::Bitmap(ns),
                        Source::Outline,
                    ])
                    .render(&mut new, gid);
                    report.check(
                        || {
                            format!(
                                "{} Render gid {gid} size {size} {ns:?}: {}",
                                f.name,
                                describe(&a, &b)
                            )
                        },
                        same_image(&a, &b),
                    );
                }
            }
        }
    }
    report.finish("bitmaps");
}
