//! COLRv1 paint graphs composited by `Render`, checked against the brush
//! geometry of the fontations test glyphs.

use glyphrast::image::{Content, Image};
use glyphrast::{Font, FontKey, RasterContext, Render, Source};
use std::sync::Arc;

const TEST_GLYPHS: &str = "tests/fonts/test_glyphs-glyf_colr_1.ttf";

fn load(path: &str) -> Font {
    let data = Arc::new(std::fs::read(path).expect("fixture"));
    Font::new(data as Arc<dyn AsRef<[u8]> + Send + Sync>, 0).expect("font")
}

fn render(cx: &mut RasterContext, font: &Font, size: f32, gid: u32) -> Option<Image> {
    let mut scaler = cx.builder(font, FontKey(1)).size(size).build();
    Render::new(&[Source::ColorOutline(0)])
        .default_color([255, 0, 0, 255])
        .render(&mut scaler, gid)
}

/// Pixel at absolute pixel coordinates with `y` up, or transparent outside.
fn at(image: &Image, x: i32, y: i32) -> [u8; 4] {
    let p = image.placement;
    let (col, row) = (x - p.left, p.top - 1 - y);
    if col < 0 || row < 0 || col >= p.width as i32 || row >= p.height as i32 {
        return [0; 4];
    }
    let i = (row as usize * p.width as usize + col as usize) * 4;
    image.data[i..i + 4].try_into().unwrap()
}

/// Pixel whose center is at font units (`fx`, `fy`) when rendered at 100px
/// with 1000 units per em.
fn at_units(image: &Image, fx: f32, fy: f32) -> [u8; 4] {
    at(image, (fx / 10.).floor() as i32, (fy / 10.).floor() as i32)
}

#[track_caller]
fn assert_close(actual: [u8; 4], expected: [u8; 4], tolerance: u8, what: &str) {
    for (a, e) in actual.iter().zip(&expected) {
        assert!(
            a.abs_diff(*e) <= tolerance,
            "{what}: got {actual:?}, expected {expected:?} within {tolerance}"
        );
    }
}

fn premultiplied(rgba: [u8; 3], alpha: f32) -> [u8; 4] {
    let a = (alpha * 255.).round();
    let px = |c: u8| (c as f32 * alpha).round() as u8;
    [px(rgba[0]), px(rgba[1]), px(rgba[2]), a as u8]
}

#[test]
fn repeating_gradients_use_the_normalized_color_line() {
    // Red to blue, repeating along x. Glyph 8 spans 100..900, glyph 9 has its
    // stops at 0.2 and 0.8 of that (so the line is 260..740), glyph 10 runs
    // 100..1300.
    let font = load(TEST_GLYPHS);
    let mut cx = RasterContext::new();
    let mix = |t: f32| {
        [
            (255. * (1. - t)).round() as u8,
            0,
            (255. * t).round() as u8,
            255,
        ]
    };
    let full = render(&mut cx, &font, 100., 8).unwrap();
    assert_close(
        at_units(&full, 505., 605.),
        mix(405. / 800.),
        3,
        "glyph 8 at 505",
    );
    let inner = render(&mut cx, &font, 100., 9).unwrap();
    assert_close(
        at_units(&inner, 505., 605.),
        mix((505. - 260.) / 480.),
        3,
        "glyph 9 at 505",
    );
    assert_close(
        at_units(&inner, 205., 605.),
        mix(((205. - 260.) / 480f32).rem_euclid(1.)),
        3,
        "glyph 9 at 205",
    );
    let long = render(&mut cx, &font, 100., 10).unwrap();
    assert_close(
        at_units(&long, 505., 605.),
        mix(405. / 1200.),
        3,
        "glyph 10 at 505",
    );
}

#[test]
fn linear_extend_modes() {
    let font = load(TEST_GLYPHS);
    let mut cx = RasterContext::new();
    // Gradient along x from 0 to 307 units: green, white at 0.5, red.
    let green = [0, 128, 0];
    let pad = render(&mut cx, &font, 100., 90).unwrap();
    let f = 55. / 307. / 0.5;
    let mid = [
        (green[0] as f32 + (255. - green[0] as f32) * f).round() as u8,
        (green[1] as f32 + (255. - green[1] as f32) * f).round() as u8,
        (green[2] as f32 + (255. - green[2] as f32) * f).round() as u8,
        255,
    ];
    assert_close(at_units(&pad, 55., 500.), mid, 3, "pad near start");
    assert_close(
        at_units(&pad, 605., 500.),
        [255, 0, 0, 255],
        1,
        "pad past end",
    );

    let repeat = render(&mut cx, &font, 100., 91).unwrap();
    let t = (605f32 / 307.).fract();
    let f = (t - 0.5) / 0.5;
    let expected = [
        255,
        (255. * (1. - f)).round() as u8,
        (255. * (1. - f)).round() as u8,
        255,
    ];
    assert_close(at_units(&repeat, 605., 500.), expected, 4, "repeat");

    let reflect = render(&mut cx, &font, 100., 92).unwrap();
    let t = 2. - 605f32 / 307.;
    let f = t / 0.5;
    let expected = [
        (green[0] as f32 + (255. - green[0] as f32) * f).round() as u8,
        (green[1] as f32 + (255. - green[1] as f32) * f).round() as u8,
        (green[2] as f32 + (255. - green[2] as f32) * f).round() as u8,
        255,
    ];
    assert_close(at_units(&reflect, 605., 500.), expected, 4, "reflect");
}

#[test]
fn radial_gradients() {
    let font = load(TEST_GLYPHS);
    let mut cx = RasterContext::new();
    // Concentric circles at (166, 768), radius 0 to 256.
    let image = render(&mut cx, &font, 100., 93).unwrap();
    assert_close(at_units(&image, 165., 765.), [0, 128, 0, 255], 8, "center");
    // Pixel centers are at multiples of 10 plus 5 font units.
    let t = ((295f32 - 166.).powi(2) + (765f32 - 768.).powi(2)).sqrt() / 256.;
    let f = (t - 0.5) / 0.5;
    let expected = [
        255,
        (255. * (1. - f)).round() as u8,
        (255. * (1. - f)).round() as u8,
        255,
    ];
    assert_close(at_units(&image, 295., 765.), expected, 3, "half radius");
    assert_close(
        at_units(&image, 905., 105.),
        [255, 0, 0, 255],
        1,
        "far outside, pad",
    );
    // Horizontal cone from (400, 500) r 100 to (700, 500) r 200, pad: the
    // start circle's interior takes the first color.
    let image = render(&mut cx, &font, 100., 96).unwrap();
    // Inside the start circle, where the largest circle through the point is
    // t = 0.0244 (the start circle itself is covered by later ones).
    assert_close(
        at_units(&image, 305., 505.),
        [12, 134, 12, 255],
        8,
        "inside start circle",
    );
    assert_close(
        at_units(&image, 895., 505.),
        [255, 0, 0, 255],
        2,
        "past end circle",
    );
}

#[test]
fn foreground_color_and_alpha() {
    let font = load(TEST_GLYPHS);
    let mut cx = RasterContext::new();
    let opaque = render(&mut cx, &font, 100., 154).unwrap();
    let faint = render(&mut cx, &font, 100., 155).unwrap();
    let p = opaque.placement;
    let mut checked = 0;
    for row in 0..p.height as i32 {
        for col in 0..p.width as i32 {
            let (x, y) = (p.left + col, p.top - 1 - row);
            let o = at(&opaque, x, y);
            if o == [255, 0, 0, 255] {
                assert_close(
                    at(&faint, x, y),
                    premultiplied([255, 0, 0], 0.3),
                    1,
                    "alpha 0.3",
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 100, "solid foreground fill not found");
}

#[test]
fn composite_modes_apply_within_the_layer_clip() {
    let font = load(TEST_GLYPHS);
    let mut cx = RasterContext::new();
    // Black cross backdrop; a yellow square (166.5..666.5 x 333.5..833.5)
    // in a layer, with a blue square (333.5..833.5 x 166.5..666.5) composited
    // onto it in the mode under test, all inside the 0..1000 clip box.
    let yellow = [0xFF, 0xDC, 0x01, 255];
    let blue = [0x68, 0xC7, 0xE8, 255];
    let black = [0, 0, 0, 255];
    let none = [0; 4];
    // (gid, yellow only at (250, 750), overlap at (500, 500) on the cross,
    //  blue only at (700, 300))
    let cases = [
        (120, "Clear", none, black, none),
        (121, "Src", none, blue, blue),
        (122, "Dest", yellow, yellow, none),
        (123, "SrcOver", yellow, blue, blue),
        (124, "DestOver", yellow, yellow, blue),
        (125, "SrcIn", none, blue, none),
        (126, "DestIn", none, yellow, none),
        (127, "SrcOut", none, black, blue),
        (128, "DestOut", yellow, black, none),
        (129, "SrcAtop", yellow, blue, none),
        (130, "DestAtop", none, yellow, blue),
        (131, "Xor", yellow, black, blue),
        (132, "Plus", yellow, [255, 255, 233, 255], blue),
        (143, "Multiply", yellow, [0x68, 0xAB, 0x01, 255], blue),
        (133, "Screen", yellow, [255, 247, 232, 255], blue),
        (141, "Difference", yellow, [0x97, 0x15, 0xE7, 255], blue),
        (135, "Darken", yellow, [0x68, 0xC7, 0x01, 255], blue),
        (136, "Lighten", yellow, [0xFF, 0xDC, 0xE8, 255], blue),
    ];
    for (gid, name, yellow_only, overlap, blue_only) in cases {
        let image = render(&mut cx, &font, 100., gid).unwrap();
        assert_close(
            at_units(&image, 255., 755.),
            yellow_only,
            2,
            &format!("{name} yellow only"),
        );
        assert_close(
            at_units(&image, 505., 505.),
            overlap,
            2,
            &format!("{name} overlap"),
        );
        assert_close(
            at_units(&image, 705., 305.),
            blue_only,
            2,
            &format!("{name} blue only"),
        );
        assert_eq!(at_units(&image, 105., 105.), none, "{name} outside");
    }
}

#[test]
fn clip_box_sizes_the_image() {
    let font = load(TEST_GLYPHS);
    let mut cx = RasterContext::new();
    for gid in [156, 157, 158, 159, 160] {
        let clip = font
            .color_glyphs()
            .get(glyphrast::read_fonts::types::GlyphId::new(gid))
            .and_then(|g| g.clip_box())
            .expect("clip box");
        let image = render(&mut cx, &font, 100., gid).unwrap();
        let p = image.placement;
        assert_eq!(p.left, (clip.x_min / 10.).floor() as i32, "gid {gid} left");
        assert_eq!(p.top, (clip.y_max / 10.).ceil() as i32, "gid {gid} top");
        assert_eq!(
            p.left + p.width as i32,
            (clip.x_max / 10.).ceil() as i32,
            "gid {gid} right"
        );
        assert_eq!(
            p.top - p.height as i32,
            (clip.y_min / 10.).floor() as i32,
            "gid {gid} bottom"
        );
    }
}

#[test]
fn translate_moves_the_paint_under_a_dest_over_layer() {
    // A half-transparent blue cross, then the same cross in orange at alpha
    // 0.7 composited underneath with DestOver, translated by (100, 0) in
    // glyph 116 and not at all in glyph 113.
    let font = load(TEST_GLYPHS);
    let mut cx = RasterContext::new();
    let base = render(&mut cx, &font, 100., 113).unwrap();
    let moved = render(&mut cx, &font, 100., 116).unwrap();
    let orange = premultiplied([0xFF, 0xA5, 0], 0.7);
    let both = [89, 58, 128, 217];
    assert_close(
        at_units(&base, 505., 405.),
        both,
        2,
        "base, blue over orange",
    );
    assert_close(
        at_units(&base, 605., 405.),
        [0; 4],
        0,
        "base, right of the cross",
    );
    assert_close(
        at_units(&moved, 605., 405.),
        orange,
        2,
        "moved, orange only",
    );
    assert_close(
        at_units(&moved, 505., 405.),
        premultiplied([0, 0, 255], 0.5),
        2,
        "moved, blue only",
    );
    assert_close(at_units(&moved, 505., 505.), both, 2, "moved, arms overlap");
}

#[test]
fn v1_matches_the_same_art_in_v0() {
    // Glyphs 168 (COLRv0) and 169 (COLRv1) paint the same seven circles; v0
    // adds the digit zero (glyph 5) and v1 the digit one (glyph 4), which
    // are skipped.
    let font = load(TEST_GLYPHS);
    let mut cx = RasterContext::new();
    for size in [32., 100.] {
        let v0 = render(&mut cx, &font, size, 168).unwrap();
        let v1 = render(&mut cx, &font, size, 169).unwrap();
        let mut scaler = cx.builder(&font, FontKey(1)).size(size).build();
        let digits: Vec<_> = [4, 5]
            .iter()
            .map(|&gid| scaler.scale_outline(gid).unwrap().bounds())
            .collect();
        assert_eq!(v1.content, Content::Color);
        // swash's v0 bounds crop the last column and row, so compare inside them.
        let p = v0.placement;
        let mut inked = 0;
        for x in p.left..p.left + p.width as i32 {
            for y in p.top - p.height as i32..p.top {
                let in_digit = digits.iter().any(|b| {
                    (x as f32 + 1.) >= b.min.x.floor()
                        && (x as f32) <= b.max.x.ceil()
                        && (y as f32 + 1.) >= b.min.y.floor()
                        && (y as f32) <= b.max.y.ceil()
                });
                if in_digit {
                    continue;
                }
                let (a, b) = (at(&v0, x, y), at(&v1, x, y));
                assert_close(b, a, 4, &format!("{size}px pixel {x},{y}"));
                inked += (a[3] > 0) as u32;
            }
        }
        assert!(inked > 100);
    }
}

#[test]
fn every_test_glyph_renders_premultiplied_or_is_rejected() {
    let font = load(TEST_GLYPHS);
    let mut cx = RasterContext::new();
    let mut rendered = 0;
    for gid in 0..font.num_glyphs() {
        for (size, hint) in [(32., false), (100., true)] {
            let mut scaler = cx.builder(&font, FontKey(1)).size(size).hint(hint).build();
            let image = Render::new(&[Source::ColorOutline(0)]).render(&mut scaler, gid);
            let Some(image) = image else {
                continue;
            };
            rendered += 1;
            assert_eq!(
                image.data.len(),
                (image.placement.width * image.placement.height * 4) as usize
            );
            for px in image.data.chunks_exact(4) {
                assert!(
                    px[..3].iter().all(|&c| c <= px[3]),
                    "gid {gid}: {px:?} is not premultiplied"
                );
            }
        }
    }
    assert!(rendered > 300, "{rendered} renders");
    // Paint graphs that cycle are rejected rather than looping.
    for gid in [178, 179] {
        assert!(render(&mut cx, &font, 32., gid).is_none(), "gid {gid}");
    }
}

#[test]
fn sweep_with_equal_angles_and_pad_splits_the_circle() {
    // Glyphs 181 and 187 sweep from blue to red over a zero-width sector, so
    // pad paints one side of the start angle blue and the other red.
    let font = load(TEST_GLYPHS);
    let mut cx = RasterContext::new();
    for gid in [181, 187] {
        let image = render(&mut cx, &font, 100., gid).unwrap();
        let mut blue = 0;
        let mut red = 0;
        let mut other = 0;
        for px in image.data.chunks_exact(4) {
            match px {
                [0, 0, 255, 255] => blue += 1,
                [255, 0, 0, 255] => red += 1,
                [_, _, _, a] if *a > 0 => other += 1,
                _ => {}
            }
        }
        assert!(blue > 500 && red > 500, "gid {gid}: blue {blue}, red {red}");
        assert!(other * 10 < blue + red, "gid {gid}: {other} mixed pixels");
    }
}
