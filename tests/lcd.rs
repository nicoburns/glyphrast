//! Properties of `Format::Lcd` output against the alpha mask of the same glyph.

use glyphrast::image::{Content, Image};
use glyphrast::zeno::{Transform, Vector};
use glyphrast::{Font, FontKey, Format, LcdFilter, RasterContext, Render, Source, SubpixelOrder};
use std::sync::Arc;

const FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
    "/usr/share/texmf/fonts/opentype/public/lm/lmroman10-regular.otf",
    "tests/fonts/NotoSansJP-Regular.subset.otf",
    "tests/fonts/vazirmatn_var_trimmed.ttf",
];

fn load(path: &str) -> Option<Font> {
    let data = Arc::new(std::fs::read(path).ok()?);
    Font::new(data as Arc<dyn AsRef<[u8]> + Send + Sync>, 0)
}

fn rgb_sum(data: &[u8]) -> u64 {
    data.chunks_exact(4)
        .map(|p| p[0] as u64 + p[1] as u64 + p[2] as u64)
        .sum()
}

/// Coverage of subpixel column `x` (absolute) of a mask rendered at three samples per pixel.
fn sample(image: &Image, x: i32, row: usize) -> u8 {
    let p = image.placement;
    let col = x - p.left;
    if col < 0 || col >= p.width as i32 {
        return 0;
    }
    image.data[row * p.width as usize + col as usize]
}

#[test]
fn lcd_packs_the_three_times_wide_mask() {
    let mut cx = RasterContext::new();
    let mut fonts_checked = 0;
    for (key, path) in FONTS.iter().enumerate() {
        let Some(font) = load(path) else {
            continue;
        };
        fonts_checked += 1;
        let num_glyphs = font.num_glyphs().min(400);
        for &(size, hint) in &[
            (10., true),
            (12., false),
            (16., true),
            (27.5, false),
            (64., true),
        ] {
            for &offset in &[Vector::new(0., 0.), Vector::new(0.4, 0.25)] {
                let mut scaler = cx
                    .builder(&font, FontKey(key as u64))
                    .size(size)
                    .hint(hint)
                    .build();
                for gid in 0..num_glyphs {
                    let what =
                        || format!("{path} gid {gid} at {size}px hint={hint} offset={offset:?}");
                    let alpha = Render::new(&[Source::Outline])
                        .offset(offset)
                        .render(&mut scaler, gid);
                    // The oracle: zeno's alpha mask of the outline stretched to three
                    // samples per pixel, which the LCD path rasterizes in one pass.
                    let wide = Render::new(&[Source::Outline])
                        .transform(Some(Transform::scale(3., 1.)))
                        .offset(Vector::new(offset.x * 3., offset.y))
                        .render(&mut scaler, gid);
                    let raw = Render::new(&[Source::Outline])
                        .format(Format::Lcd {
                            filter: LcdFilter::None,
                            order: SubpixelOrder::Rgb,
                        })
                        .offset(offset)
                        .render(&mut scaler, gid);
                    let lcd = Render::new(&[Source::Outline])
                        .format(Format::LCD)
                        .offset(offset)
                        .render(&mut scaler, gid);
                    let (alpha, wide, raw, lcd) = match (alpha, wide, raw, lcd) {
                        (Some(a), Some(w), Some(r), Some(l)) => (a, w, r, l),
                        (None, None, None, None) => continue,
                        _ => panic!("{}: only some formats produced an image", what()),
                    };
                    for image in [&raw, &lcd] {
                        assert_eq!(image.content, Content::SubpixelMask, "{}", what());
                        let p = image.placement;
                        assert_eq!(p.height, alpha.placement.height, "{}: height", what());
                        assert_eq!(p.top, alpha.placement.top, "{}: top", what());
                        assert_eq!(
                            image.data.len(),
                            (p.width * p.height * 4) as usize,
                            "{}",
                            what()
                        );
                    }
                    let (ap, wp, rp, lp) = (
                        alpha.placement,
                        wide.placement,
                        raw.placement,
                        lcd.placement,
                    );
                    if ap.width == 0 {
                        continue;
                    }
                    // Unfiltered: pixels cover exactly the wide mask's columns.
                    assert_eq!(rp.left, wp.left.div_euclid(3), "{}: raw left", what());
                    assert_eq!(
                        rp.left + rp.width as i32,
                        (wp.left + wp.width as i32 + 2).div_euclid(3),
                        "{}: raw right",
                        what()
                    );
                    // Filtered: the default filter spills two subpixels past each side.
                    assert_eq!(lp.left, (wp.left - 2).div_euclid(3), "{}: lcd left", what());
                    assert_eq!(
                        lp.left + lp.width as i32,
                        (wp.left + wp.width as i32 + 4).div_euclid(3),
                        "{}: lcd right",
                        what()
                    );
                    assert!(
                        lp.left >= ap.left - 1 && lp.left <= ap.left,
                        "{}: {lp:?} vs {ap:?}",
                        what()
                    );
                    let mut raw_ink = 0u64;
                    let mut lcd_ink = 0u64;
                    for row in 0..wp.height as usize {
                        for (x, px) in raw.data[row * rp.width as usize * 4..]
                            [..rp.width as usize * 4]
                            .chunks_exact(4)
                            .enumerate()
                        {
                            let sx = (rp.left + x as i32) * 3;
                            let expected = [
                                sample(&wide, sx, row),
                                sample(&wide, sx + 1, row),
                                sample(&wide, sx + 2, row),
                            ];
                            assert_eq!(&px[..3], &expected, "{}: raw pixel {x} row {row}", what());
                            raw_ink += expected.iter().map(|&v| v as u64).sum::<u64>();
                        }
                        for (x, px) in lcd.data[row * lp.width as usize * 4..]
                            [..lp.width as usize * 4]
                            .chunks_exact(4)
                            .enumerate()
                        {
                            let sx = (lp.left + x as i32) * 3;
                            for (c, &v) in px[..3].iter().enumerate() {
                                // Default filter of the wide mask, in 1/256 units.
                                let s = sx + c as i32;
                                let sum: u32 = [0x08, 0x4D, 0x56, 0x4D, 0x08]
                                    .iter()
                                    .enumerate()
                                    .map(|(k, w)| w * sample(&wide, s + k as i32 - 2, row) as u32)
                                    .sum();
                                assert_eq!(
                                    v as u32,
                                    sum >> 8,
                                    "{}: lcd pixel {x} channel {c} row {row}",
                                    what()
                                );
                                lcd_ink += v as u64;
                            }
                            let mean = (px[0] as u32 + px[1] as u32 + px[2] as u32 + 1) / 3;
                            assert_eq!(px[3] as u32, mean, "{}: alpha of {px:?}", what());
                        }
                    }
                    // The filter is normalised: it only loses the truncated 1/256ths.
                    let samples = lcd.data.len() as u64 / 4 * 3;
                    assert!(
                        lcd_ink <= raw_ink && raw_ink - lcd_ink <= samples,
                        "{}: ink {raw_ink} vs {lcd_ink}",
                        what()
                    );
                }
            }
        }
    }
    assert!(fonts_checked >= 2, "test fonts missing");
}

#[test]
fn filters_and_orders() {
    let font = load("tests/fonts/tinos_subset.ttf").expect("fixture");
    let mut cx = RasterContext::new();
    let mut scaler = cx.builder(&font, FontKey(1)).size(16.).hint(true).build();
    let gid = (0..font.num_glyphs())
        .find(|&gid| {
            Render::new(&[Source::Outline])
                .render(&mut scaler, gid)
                .is_some_and(|image| image.placement.width > 0)
        })
        .expect("a glyph with ink");
    let render = |scaler: &mut _, filter, order| {
        Render::new(&[Source::Outline])
            .format(Format::Lcd { filter, order })
            .render(scaler, gid)
            .expect("outline")
    };
    let default = render(&mut scaler, LcdFilter::Default, SubpixelOrder::Rgb);
    let light = render(&mut scaler, LcdFilter::Light, SubpixelOrder::Rgb);
    let none = render(&mut scaler, LcdFilter::None, SubpixelOrder::Rgb);
    let bgr = render(&mut scaler, LcdFilter::Default, SubpixelOrder::Bgr);

    assert!(none.placement.width > 0);
    assert!(default.placement.width >= light.placement.width);
    assert!(light.placement.width >= none.placement.width);
    let (d, b) = (default.placement, bgr.placement);
    assert_eq!(
        (d.left, d.top, d.width, d.height),
        (b.left, b.top, b.width, b.height)
    );
    for (p, q) in default.data.chunks_exact(4).zip(bgr.data.chunks_exact(4)) {
        assert_eq!([p[2], p[1], p[0], p[3]], q);
    }
    // Filtering only redistributes coverage between neighbouring subpixels.
    let raw = rgb_sum(&none.data);
    for (name, image) in [("default", &default), ("light", &light)] {
        let filtered = rgb_sum(&image.data);
        let slack = image.data.len() as u64 / 4 * 3;
        assert!(
            raw.abs_diff(filtered) <= slack,
            "{name}: {raw} vs {filtered}"
        );
        assert!(filtered <= raw, "{name}: filtering added ink");
    }
    // The unfiltered mask has every channel equal to its subpixel coverage,
    // which the default filter smooths: no pixel of a stem is more extreme than its raw sample.
    let max_raw = none
        .data
        .chunks_exact(4)
        .map(|p| p[0].max(p[1]).max(p[2]))
        .max()
        .unwrap();
    assert_eq!(
        max_raw, 255,
        "16px Tinos glyph 1 has a fully covered subpixel"
    );
}
