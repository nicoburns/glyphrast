//! FIR filtering and packing of a coverage mask rasterized at three samples
//! per pixel into an RGBA LCD image, following FreeType's `ftlcdfil`.

use crate::render::{LcdFilter, SubpixelOrder};
use alloc::vec::Vec;
use zeno::Placement;

impl LcdFilter {
    /// Five taps over neighbouring subpixels, summing to 256.
    fn weights(self) -> [u32; 5] {
        match self {
            Self::Default => [0x08, 0x4D, 0x56, 0x4D, 0x08],
            Self::Light => [0x00, 0x55, 0x56, 0x55, 0x00],
            Self::None => [0, 0, 0x100, 0, 0],
        }
    }

    /// Subpixels the filter spills past each side of the coverage.
    fn padding(self) -> i32 {
        match self {
            Self::Default => 2,
            Self::Light => 1,
            Self::None => 0,
        }
    }
}

/// Filters `coverage`, a tightly packed mask whose `placement` is in
/// subpixels, into `out` as RGBA pixels and returns the placement in pixels.
/// `row` is scratch space for one padded row.
pub(crate) fn pack(
    coverage: &[u8],
    placement: Placement,
    filter: LcdFilter,
    order: SubpixelOrder,
    row: &mut Vec<u8>,
    out: &mut Vec<u8>,
) -> Placement {
    out.clear();
    if placement.width == 0 || placement.height == 0 {
        return Placement {
            left: placement.left.div_euclid(3),
            ..placement
        };
    }
    let weights = filter.weights();
    let pad = filter.padding();
    let sub_left = placement.left;
    let sub_width = placement.width as i32;
    let height = placement.height as usize;
    let left = (sub_left - pad).div_euclid(3);
    let right = (sub_left + sub_width + pad + 2).div_euclid(3);
    let width = (right - left) as usize;
    let span = width * 3;
    // Two zero taps either side of the pixel span.
    row.clear();
    row.resize(span + 4, 0);
    let start = (sub_left - left * 3) as usize + 2;
    let (r, g, b) = match order {
        SubpixelOrder::Rgb => (0, 1, 2),
        SubpixelOrder::Bgr => (2, 1, 0),
    };
    out.resize(width * height * 4, 0);
    for (src, dst) in coverage
        .chunks_exact(sub_width as usize)
        .zip(out.chunks_exact_mut(width * 4))
        .take(height)
    {
        row.fill(0);
        row[start..start + src.len()].copy_from_slice(src);
        let mut taps = row.windows(5);
        for px in dst.chunks_exact_mut(4) {
            let mut ch = [0u8; 3];
            for ch in &mut ch {
                let taps = taps.next().unwrap_or_default();
                let sum: u32 = taps.iter().zip(&weights).map(|(&s, w)| s as u32 * w).sum();
                *ch = (sum >> 8).min(255) as u8;
            }
            px[r] = ch[0];
            px[g] = ch[1];
            px[b] = ch[2];
            px[3] = ((ch[0] as u32 + ch[1] as u32 + ch[2] as u32 + 1) / 3) as u8;
        }
    }
    Placement {
        left,
        top: placement.top,
        width: width as u32,
        height: placement.height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_subpixel(filter: LcdFilter, order: SubpixelOrder) -> (Vec<u8>, Placement) {
        let placement = Placement {
            left: 0,
            top: 1,
            width: 1,
            height: 1,
        };
        let mut out = Vec::new();
        let placement = pack(&[255], placement, filter, order, &mut Vec::new(), &mut out);
        (out, placement)
    }

    #[test]
    fn default_filter_spreads_into_the_left_pixel() {
        let (out, placement) = one_subpixel(LcdFilter::Default, SubpixelOrder::Rgb);
        assert_eq!((placement.left, placement.top), (-1, 1));
        assert_eq!((placement.width, placement.height), (2, 1));
        assert_eq!(out, [0, 7, 76, 28, 85, 76, 7, 56]);
    }

    #[test]
    fn light_filter_spills_one_subpixel() {
        let (out, placement) = one_subpixel(LcdFilter::Light, SubpixelOrder::Rgb);
        assert_eq!((placement.left, placement.width), (-1, 2));
        assert_eq!(out, [0, 0, 84, 28, 85, 84, 0, 56]);
    }

    #[test]
    fn unfiltered_packs_raw_coverage() {
        let (out, placement) = one_subpixel(LcdFilter::None, SubpixelOrder::Rgb);
        assert_eq!((placement.left, placement.width), (0, 1));
        assert_eq!(out, [255, 0, 0, 85]);
    }

    #[test]
    fn bgr_swaps_the_outer_channels() {
        let (rgb, _) = one_subpixel(LcdFilter::Default, SubpixelOrder::Rgb);
        let (bgr, _) = one_subpixel(LcdFilter::Default, SubpixelOrder::Bgr);
        for (p, q) in rgb.chunks_exact(4).zip(bgr.chunks_exact(4)) {
            assert_eq!([p[2], p[1], p[0], p[3]], q);
        }
    }

    #[test]
    fn negative_and_unaligned_left_edges() {
        let placement = Placement {
            left: -4,
            top: 0,
            width: 2,
            height: 2,
        };
        let mut out = Vec::new();
        let p = pack(
            &[255, 255, 0, 255],
            placement,
            LcdFilter::None,
            SubpixelOrder::Rgb,
            &mut Vec::new(),
            &mut out,
        );
        assert_eq!((p.left, p.width, p.height), (-2, 2, 2));
        assert_eq!(
            out,
            [0, 0, 255, 85, 255, 0, 0, 85, 0, 0, 0, 0, 255, 0, 0, 85]
        );
    }

    #[test]
    fn empty_coverage_gives_an_empty_image() {
        let mut out = alloc::vec![1, 2, 3, 4];
        let p = pack(
            &[],
            Placement::default(),
            LcdFilter::Default,
            SubpixelOrder::Rgb,
            &mut Vec::new(),
            &mut out,
        );
        assert_eq!((p.width, p.height), (0, 0));
        assert!(out.is_empty());
    }
}
