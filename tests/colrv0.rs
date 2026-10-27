//! A COLRv0 image must cover every layer's mask, whatever the fractional offset.
//!
//! swash sized the image from `ceil(min.y)` and the unrounded width and height,
//! which cropped the bottom row and right column of most color glyphs
//! (dfrg/swash#117).

use glyphrast::zeno::{Format, Placement, Vector};
use glyphrast::{Font, FontKey, RasterContext, Render, Source};
use std::sync::Arc;

/// Layer glyphs of each base glyph in `tests/fonts/colrv0_dejavu.ttf`.
const LAYERS: &[(char, &[&str])] = &[
    ('A', &["o", "A", "period"]),
    ('B', &["O", "B"]),
    ('C', &["c", "i", "hyphen"]),
    ('a', &["a"]),
];

fn union(a: Placement, b: Placement) -> Placement {
    let left = a.left.min(b.left);
    let right = (a.left + a.width as i32).max(b.left + b.width as i32);
    let top = a.top.max(b.top);
    let bottom = (a.top - a.height as i32).min(b.top - b.height as i32);
    Placement {
        left,
        top,
        width: (right - left) as u32,
        height: (top - bottom) as u32,
    }
}

#[test]
fn color_image_covers_all_layers() {
    let path = format!(
        "{}/tests/fonts/colrv0_dejavu.ttf",
        env!("CARGO_MANIFEST_DIR")
    );
    let data = std::fs::read(path).unwrap();
    let font = Font::new(Arc::new(data) as Arc<dyn AsRef<[u8]> + Send + Sync>, 0).unwrap();
    let charmap = font.charmap();
    let mut cx = RasterContext::new();
    for size in [8.3f32, 12., 16., 24.5, 48.] {
        for offset in [Vector::ZERO, Vector::new(0.4, 0.7), Vector::new(0.9, 0.1)] {
            let mut scaler = cx.builder(&font, FontKey(1)).size(size).build();
            for (ch, layers) in LAYERS {
                let gid = charmap.map_unicode(*ch as u32).unwrap().to_u32();
                let color = Render::new(&[Source::ColorOutline(0)])
                    .offset(offset)
                    .render(&mut scaler, gid)
                    .unwrap();
                let mut expected: Option<Placement> = None;
                for name in *layers {
                    let layer_gid = font.glyph_by_name(name).unwrap().to_u32();
                    let mask = Render::new(&[Source::Outline])
                        .format(Format::Alpha)
                        .offset(offset)
                        .render(&mut scaler, layer_gid)
                        .unwrap();
                    expected = Some(match expected {
                        Some(p) => union(p, mask.placement),
                        None => mask.placement,
                    });
                }
                let expected = expected.unwrap();
                assert_eq!(
                    format!("{:?}", color.placement),
                    format!("{expected:?}"),
                    "{ch} at {size}px offset {offset:?}"
                );
                assert_eq!(
                    color.data.len(),
                    (expected.width * expected.height * 4) as usize
                );
            }
        }
    }
}
