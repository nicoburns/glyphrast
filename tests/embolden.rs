//! Faux bold must thicken every glyph, including ones with several contours.
//!
//! swash computed the outline orientation over all contours as one polygon, so
//! glyphs such as accented capitals (dfrg/swash#146, #142) were moved inward
//! and lost about half their coverage.

use glyphrast::zeno::{Format, Verb};
use glyphrast::{Font, FontKey, RasterContext, Render, Source};
use std::sync::Arc;

const FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
    "/usr/share/texmf/fonts/opentype/public/lm/lmroman10-regular.otf",
];

#[test]
fn embolden_never_thins_a_glyph() {
    let mut cx = RasterContext::new();
    let mut checked = 0;
    for path in FONTS {
        let Ok(data) = std::fs::read(path) else {
            eprintln!("skipping: {path} not installed");
            continue;
        };
        let font = Font::new(Arc::new(data) as Arc<dyn AsRef<[u8]> + Send + Sync>, 0).unwrap();
        for gid in 0..font.num_glyphs().min(600) {
            let mut scaler = cx.builder(&font, FontKey(1)).size(48.).build();
            let Some(outline) = scaler.scale_outline(gid) else {
                continue;
            };
            if !outline.verbs().contains(&Verb::MoveTo) {
                continue;
            }
            let render = |scaler: &mut glyphrast::Scaler<'_>, embolden: f32| {
                let image = Render::new(&[Source::Outline])
                    .format(Format::Alpha)
                    .embolden(embolden)
                    .render(scaler, gid)
                    .unwrap();
                let coverage: u64 = image.data.iter().map(|&c| c as u64).sum();
                (coverage, image.placement.width, image.placement.height)
            };
            let (thin, w0, h0) = render(&mut scaler, 0.);
            let (bold, w1, h1) = render(&mut scaler, 1.);
            assert!(
                bold >= thin,
                "{path} gid {gid}: embolden reduced coverage {thin} -> {bold}"
            );
            assert!(
                w1 >= w0 && h1 >= h0 && w1 <= w0 + 3 && h1 <= h0 + 3,
                "{path} gid {gid}: emboldened image {w0}x{h0} -> {w1}x{h1}"
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "no fonts available");
}
