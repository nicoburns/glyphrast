# glyphrast

Glyph scaling and rasterization on top of [`read-fonts`](https://crates.io/crates/read-fonts),
[`skrifa`](https://crates.io/crates/skrifa) and [`zeno`](https://crates.io/crates/zeno).

This is a port of the `scale` module of [swash](https://github.com/dfrg/swash) 0.2.10. The
outline, image and rendering types keep swash's shapes so existing users can switch by changing
how they pass the font. Fonts are `read_fonts::model::Font`s, and hinted instances are cached
`skrifa::ScalerFont`s, so the table parsers, font cache and hinting cache that swash carried are
gone.

```rust
use glyphrast::{Font, FontKey, RasterContext, Render, Source, StrikeWith};

let mut context = RasterContext::new();
let mut scaler = context.builder(&font, FontKey(1)).size(14.0).hint(true).build();
let image = Render::new(&[
    Source::ColorOutline(0),
    Source::ColorBitmap(StrikeWith::BestFit),
    Source::Outline,
])
.render(&mut scaler, glyph_id);
```

## Differences from swash

- The font argument is a `read_fonts::model::Font` plus a caller-supplied `FontKey`, which only
  selects cache entries. Variation coordinates given to the builder replace the font's location.
- Glyph identifiers are `u32`.
- `ScalerBuilder::variations` (by tag) is gone; build the location with
  `Font::instance_builder()` or pass `normalized_coords`.
- `ScalerBuilder::hinting` exposes skrifa's engine and raster target.
- `Scaler::scale_outline_into` returns the hinted advance and bearing instead of `bool`.
- A glyph whose embedded bitmap record is empty yields no bitmap; swash decoded the next glyph's
  image for it.
- `Content::Color` data is premultiplied RGBA. swash returned straight alpha for PNG bitmaps and
  passed raw 32-bit CBDT data through in BGRA order (its COLRv0 compositing was already
  premultiplied).

Everything else, including swash's hinting configuration and its bitmap and COLRv0 compositing,
reproduces swash's output byte for byte (see `tests/golden.rs`, which compares against swash
0.2.10 from crates.io; set `NOTO_COLOR_EMOJI=/path/to/NotoColorEmoji.ttf` to include that font).

## COLRv1

`Source::ColorOutline` renders a glyph's COLRv1 paint graph when it has one, and its COLRv0
layers otherwise. The paint callbacks from `read_fonts` drive a small software compositor:
a stack of premultiplied RGBA layers the size of the glyph's clip box (or painted bounds),
clip masks rasterized with zeno, solid, linear, radial and sweep brushes with the three
extend modes evaluated per pixel, and the Porter-Duff, separable and HSL composite modes.
Palette index `0xFFFF` takes `Render::default_color`. `Scaler::scale_color_outline` still
returns COLRv0 layers only, since a paint graph has no outline representation.

## Features

- `std` (default): use the standard library.
- `libm`: floating point functions from `libm` for `no_std` builds.
- `png` (default): decode embedded PNG bitmaps with the `png` crate (requires `std`).
- `yazi-png`: decode embedded PNG bitmaps with swash's decoder on `yazi` (`no_std`-capable).
  Enable only one of `png` and `yazi-png`.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option. Derived from
swash, copyright Chad Brokaw, under the same licenses.
