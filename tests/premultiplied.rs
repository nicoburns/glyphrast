//! Every `Content::Color` image is premultiplied: no channel exceeds alpha.

use glyphrast::image::Content;
use glyphrast::{Font, FontKey, RasterContext, Render, Source, StrikeWith};
use std::sync::Arc;

fn load(name: &str) -> Font {
    let path = format!("{}/tests/fonts/{name}", env!("CARGO_MANIFEST_DIR"));
    let data = std::fs::read(path).unwrap();
    Font::new(Arc::new(data) as Arc<dyn AsRef<[u8]> + Send + Sync>, 0).unwrap()
}

fn check(image: &glyphrast::image::Image, what: &str) -> usize {
    assert_eq!(image.content, Content::Color, "{what}");
    let mut translucent = 0;
    for px in image.data.chunks_exact(4) {
        assert!(
            px[..3].iter().all(|&c| c <= px[3]),
            "{what}: {px:?} is not premultiplied"
        );
        if px[3] != 0 && px[3] != 255 {
            translucent += 1;
        }
    }
    translucent
}

#[test]
fn bitmaps_and_color_outlines_are_premultiplied() {
    let mut cx = RasterContext::new();
    let mut translucent = 0;
    for (name, sizes) in [
        ("cbdt.ttf", &[0., 16., 20., 64., 100.][..]),
        ("noto_handwriting-sbix.ttf", &[0., 24., 40.][..]),
    ] {
        let font = load(name);
        for &size in sizes {
            let mut scaler = cx.builder(&font, FontKey(1)).size(size).build();
            for gid in 0..font.num_glyphs() {
                let Some(image) = scaler.scale_color_bitmap(gid, StrikeWith::BestFit) else {
                    continue;
                };
                translucent += check(&image, &format!("{name} gid {gid} at {size}px"));
            }
        }
    }
    let font = load("colrv0_dejavu.ttf");
    let mut scaler = cx.builder(&font, FontKey(2)).size(24.).build();
    for gid in 0..font.num_glyphs() {
        let Some(image) = Render::new(&[Source::ColorOutline(0)])
            .default_color([200, 100, 50, 128])
            .render(&mut scaler, gid)
        else {
            continue;
        };
        translucent += check(&image, &format!("colrv0 gid {gid}"));
    }
    assert!(
        translucent > 100,
        "expected translucent pixels to exercise the check"
    );
}
