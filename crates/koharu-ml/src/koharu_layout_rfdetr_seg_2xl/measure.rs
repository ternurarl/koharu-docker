//! The shape of lettering on the page under its mask: `glyph_mask` fits a
//! text or onomatopoeia mask to the glyphs it covers.
//!
//! Ported from upstream koharu-rs/koharu 4323cf90, e31c3e2d, and 4eefd2f0's
//! `koharu_layout_highres/measure.rs`, with two deliberate divergences: the
//! port keeps only `glyph_mask`, so `ink` takes the detection's box directly
//! instead of an `Option` it always set, and the fitted block is not measured
//! for typography, which this fork measures in `koharu-pipeline`'s detection
//! stage.

use image::{GrayImage, ImageBuffer, Luma, RgbImage, imageops};
use imageproc::{
    distance_transform::Norm,
    morphology::{close, dilate},
    region_labelling::{Connectivity, connected_components},
};
use koharu_leptonica::otsu_adaptive_threshold;

use super::processor::KoharuLayoutMask;

/// A text or onomatopoeia mask fitted to its glyphs: RF-DETR's mask within
/// its box, which hugs each stroke and its outline but misses counters and
/// parts of strokes, with its block's ink, widened by 2 pixels per 1024 of the
/// page's longer side for anti-aliased edges. The paper or art between glyphs
/// stays out, so inpainting redraws only the lettering.
pub fn glyph_mask(mask: &KoharuLayoutMask, bbox: [f32; 4], page: &RgbImage) -> KoharuLayoutMask {
    let (page_width, page_height) = (page.width(), page.height());
    // RF-DETR's mask can stop a few pixels short of a glyph's ends, and LaMa
    // grows even a sliver of ink left outside into a stroke, so the ink may
    // reach as far past the block as the block's closing radius.
    let reach = scaled(6.0, page_width, page_height);
    // RF-DETR predicts each query's mask over the whole page, and a lettering
    // mask can take in specks of other lettering far from its box, whose
    // surroundings then sway the ink split. Its own glyphs can run past the
    // box's ends too, but by less than a glyph, the box's narrower side, and
    // past its sides by no more than the closing radius: a glyph's width
    // further across is the next line, often another lettering's, whose
    // clean-up would cover that lettering's translation.
    let [left, top, right, bottom] = bbox;
    let (width, height) = (right - left, bottom - top);
    let (across, down) = match height >= width {
        true => (reach as f32, width),
        false => (height, reach as f32),
    };
    let clipped = GrayImage::from_fn(mask.width, mask.height, |x, y| {
        let (page_x, page_y) = ((mask.x + x) as f32, (mask.y + y) as f32);
        let kept = mask.pixels[(y * mask.width + x) as usize] != 0
            && (left - across..right + across).contains(&page_x)
            && (top - down..bottom + down).contains(&page_y);
        Luma([if kept { 255 } else { 0 }])
    });
    // A part of the mask that never comes within the closing radius of the
    // box is a speck of other lettering too, such as a balloon's glyph above
    // a tall sound effect, whose clean-up would cover that balloon's
    // translation; a glyph of its own that the box cuts off reaches into it.
    let parts = connected_components(&clipped, Connectivity::Eight, Luma([0]));
    let labels = parts.pixels().map(|part| part.0[0]).max().unwrap_or(0) as usize;
    // Part 0 is the background, never near.
    let mut near = vec![false; labels + 1];
    let radius = reach as f32;
    for (x, y, part) in parts.enumerate_pixels() {
        let (page_x, page_y) = ((mask.x + x) as f32, (mask.y + y) as f32);
        if part.0[0] != 0
            && (left - radius..right + radius).contains(&page_x)
            && (top - radius..bottom + radius).contains(&page_y)
        {
            near[part.0[0] as usize] = true;
        }
    }
    let clipped = GrayImage::from_fn(mask.width, mask.height, |x, y| {
        Luma([if near[parts.get_pixel(x, y).0[0] as usize] {
            255
        } else {
            0
        }])
    });
    let mask = crop(
        &clipped,
        i64::from(mask.x),
        i64::from(mask.y),
        page_width,
        page_height,
    );
    let block = block(&mask, page_width, page_height);
    let Some((ink, [left, top])) = ink(&block, page, reach, bbox) else {
        return mask;
    };
    let margin = scaled(2.0, page_width, page_height);
    let (left, top) = (
        i64::from(left) - i64::from(margin),
        i64::from(top) - i64::from(margin),
    );
    let mut canvas = GrayImage::new(ink.width() + 2 * margin, ink.height() + 2 * margin);
    for (x, y, pixel) in ink.enumerate_pixels() {
        if pixel.0[0] != 0 {
            canvas.put_pixel(x + margin, y + margin, Luma([255]));
        }
    }
    for (index, _) in mask
        .pixels
        .iter()
        .enumerate()
        .filter(|(_, value)| **value != 0)
    {
        let x = i64::from(mask.x + index as u32 % mask.width) - left;
        let y = i64::from(mask.y + index as u32 / mask.width) - top;
        if (0..i64::from(canvas.width())).contains(&x)
            && (0..i64::from(canvas.height())).contains(&y)
        {
            canvas.put_pixel(x as u32, y as u32, Luma([255]));
        }
    }
    // Inside the box, what the glyphs enclose is a counter or, in lettering
    // outlined in a lighter colour, the glyph's own fill; the ink split takes
    // only the outline there.
    let [box_left, box_top, box_right, box_bottom] = bbox;
    let mut enclosed = canvas.clone();
    fill_holes(&mut enclosed);
    for (x, y, pixel) in enclosed.enumerate_pixels() {
        let (page_x, page_y) = ((left + i64::from(x)) as f32, (top + i64::from(y)) as f32);
        if pixel.0[0] != 0
            && (box_left..box_right).contains(&page_x)
            && (box_top..box_bottom).contains(&page_y)
        {
            canvas.put_pixel(x, y, Luma([255]));
        }
    }
    let fitted = dilate(&canvas, Norm::L2, margin as u8);
    crop(&fitted, left, top, page_width, page_height)
}

/// Fills what `canvas`'s marked pixels enclose: unmarked areas its edge
/// can't reach.
fn fill_holes(canvas: &mut GrayImage) {
    let (width, height) = (canvas.width() as usize, canvas.height() as usize);
    if width == 0 || height == 0 {
        return;
    }
    // The background is every pixel that isn't 255, its parts joined 4 ways
    // where their values match; a part reaching the edge is outside, and the
    // rest are holes. A flood from the edge finds the parts outside.
    let pixels: &mut [u8] = canvas.as_mut();
    let mut outside = vec![false; width * height];
    let mut queue = Vec::new();
    let edge = (0..width)
        .flat_map(|x| [x, (height - 1) * width + x])
        .chain((0..height).flat_map(|y| [y * width, y * width + width - 1]));
    for at in edge {
        if pixels[at] != u8::MAX && !outside[at] {
            outside[at] = true;
            queue.push(at);
        }
    }
    while let Some(at) = queue.pop() {
        let (x, y) = (at % width, at / width);
        let neighbours = [
            (x > 0).then(|| at - 1),
            (x + 1 < width).then(|| at + 1),
            (y > 0).then(|| at - width),
            (y + 1 < height).then(|| at + width),
        ];
        for next in neighbours.into_iter().flatten() {
            if !outside[next] && pixels[next] == pixels[at] {
                outside[next] = true;
                queue.push(next);
            }
        }
    }
    for (pixel, outside) in pixels.iter_mut().zip(outside) {
        if *pixel != u8::MAX && !outside {
            *pixel = u8::MAX;
        }
    }
}

/// The lettering's block: `mask` closed and with its holes filled, so its
/// lines are solid and its outline is their sides and ends. The radius is
/// the pipeline's text-mask rule, 6 pixels per 1024 of the page's longer
/// side.
fn block(mask: &KoharuLayoutMask, page_width: u32, page_height: u32) -> KoharuLayoutMask {
    if mask.width == 0 || mask.height == 0 {
        return mask.clone();
    }
    let radius = scaled(6.0, page_width, page_height);
    // Twice the radius, so the dilation never reaches the canvas's edge,
    // where imageproc's erosion would keep it.
    let pad = 2 * radius;
    let mut canvas = GrayImage::new(mask.width + 2 * pad, mask.height + 2 * pad);
    for (index, _) in mask
        .pixels
        .iter()
        .enumerate()
        .filter(|(_, value)| **value != 0)
    {
        let (x, y) = (index as u32 % mask.width, index as u32 / mask.width);
        canvas.put_pixel(x + pad, y + pad, Luma([255]));
    }
    let mut closed = close(&canvas, Norm::LInf, radius as u8);
    fill_holes(&mut closed);
    let (left, top) = (
        i64::from(mask.x) - i64::from(pad),
        i64::from(mask.y) - i64::from(pad),
    );
    crop(&closed, left, top, page_width, page_height)
}

/// The page's crop around the block and `bbox`, `reach` pixels wider than
/// both on every side, its ink, and the crop's place on the page. The ink is
/// the side of one Otsu split of the crop that isn't the paper: inside the
/// block, in the glyph ends it cut off, and on plain paper in the
/// glyph-sized parts wholly inside the box or in line with it. The paper is
/// what the crop shows around the block, or with none around, the side
/// covering more of the block, so light lettering on dark counts too. `None`
/// when the block holds no ink.
fn ink(
    block: &KoharuLayoutMask,
    page: &RgbImage,
    reach: u32,
    bbox: [f32; 4],
) -> Option<(GrayImage, [u32; 2])> {
    if block.width == 0 || block.height == 0 {
        return None;
    }
    let [left, top, right, bottom] = bbox;
    let boxed = [
        left.floor().max(0.0) as u32,
        top.floor().max(0.0) as u32,
        (right.ceil().max(0.0) as u32).min(page.width()),
        (bottom.ceil().max(0.0) as u32).min(page.height()),
    ];
    // The box, lengthened at both ends of its line by the line's width: a
    // glyph the box cuts off lies in line with the others.
    let lined = {
        let [box_left, box_top, box_right, box_bottom] = boxed;
        let (box_width, box_height) = (
            box_right.saturating_sub(box_left),
            box_bottom.saturating_sub(box_top),
        );
        let line = box_width.min(box_height);
        if box_height >= box_width {
            [
                box_left,
                box_top.saturating_sub(line),
                box_right,
                (box_bottom + line).min(page.height()),
            ]
        } else {
            [
                box_left.saturating_sub(line),
                box_top,
                (box_right + line).min(page.width()),
                box_bottom,
            ]
        }
    };
    let (mut left, mut top) = (block.x.saturating_sub(reach), block.y.saturating_sub(reach));
    let (mut right, mut bottom) = (
        (block.x + block.width + reach).min(page.width()),
        (block.y + block.height + reach).min(page.height()),
    );
    // `reach` past the lengthened box too, so ink crossing its edge shows
    // outside it.
    let [box_left, box_top, box_right, box_bottom] = lined;
    (left, top) = (
        left.min(box_left.saturating_sub(reach)),
        top.min(box_top.saturating_sub(reach)),
    );
    (right, bottom) = (
        right.max((box_right + reach).min(page.width())),
        bottom.max((box_bottom + reach).min(page.height())),
    );
    let (width, height) = (right - left, bottom - top);
    let crop = imageops::crop_imm(page, left, top, width, height).to_image();
    let (offset_x, offset_y) = (block.x - left, block.y - top);
    let inside = |x: u32, y: u32| {
        (offset_x..offset_x + block.width).contains(&x)
            && (offset_y..offset_y + block.height).contains(&y)
            && block.pixels[((y - offset_y) * block.width + x - offset_x) as usize] != 0
    };
    // One Otsu split of the block and the detection's box, without the
    // reach around them, where art dark enough to take the split from faint
    // lettering can lie. One tile covers the region, so the split is a
    // threshold, read back from it to divide the whole crop.
    let gray = imageops::grayscale(&crop);
    let [region_left, region_top, region_right, region_bottom] = [
        box_left.min(block.x).max(left),
        box_top.min(block.y).max(top),
        box_right.max(block.x + block.width).min(right),
        box_bottom.max(block.y + block.height).min(bottom),
    ];
    let (region_width, region_height) = (region_right - region_left, region_bottom - region_top);
    let region = imageops::crop_imm(
        &gray,
        region_left - left,
        region_top - top,
        region_width,
        region_height,
    )
    .to_image();
    let split = otsu_adaptive_threshold(
        &region,
        region_width.max(16),
        region_height.max(16),
        0,
        0,
        0.1,
    )
    .ok()?;
    let threshold = region
        .pixels()
        .zip(split.pixels())
        .filter(|(_, below)| below.0[0] != 0)
        .map(|(value, _)| u16::from(value.0[0]) + 1)
        .max()
        .unwrap_or(0);
    let dark = GrayImage::from_fn(width, height, |x, y| {
        Luma([if u16::from(gray.get_pixel(x, y).0[0]) < threshold {
            255
        } else {
            0
        }])
    });
    let (mut around, mut dark_around, mut covered, mut dark_covered) = (0u32, 0u32, 0u32, 0u32);
    for (x, y, pixel) in dark.enumerate_pixels() {
        let is_dark = u32::from(pixel.0[0] != 0);
        if inside(x, y) {
            covered += 1;
            dark_covered += is_dark;
        } else {
            around += 1;
            dark_around += is_dark;
        }
    }
    let light_ink = if around > 0 {
        2 * dark_around > around
    } else {
        2 * dark_covered > covered
    };
    let is_ink = |x: u32, y: u32| (dark.get_pixel(x, y).0[0] != 0) != light_ink;

    // Past the block, ink that joins the block's and ends within `reach` is a
    // glyph's end the mask cut off; ink that runs on is an outline, a border,
    // or art.
    let shape = GrayImage::from_fn(width, height, |x, y| {
        Luma([if inside(x, y) { 255 } else { 0 }])
    });
    let near = dilate(&shape, Norm::LInf, reach as u8);
    let ring = GrayImage::from_fn(width, height, |x, y| {
        Luma([
            if near.get_pixel(x, y).0[0] != 0 && !inside(x, y) && is_ink(x, y) {
                255
            } else {
                0
            },
        ])
    });
    let parts = connected_components(&ring, Connectivity::Eight, Luma([0]));
    // A flag for each part, by label: connected_components numbers them from
    // 1 up.
    let labels = |parts: &ImageBuffer<Luma<u32>, Vec<u32>>| {
        parts.pixels().map(|part| part.0[0]).max().unwrap_or(0) as usize + 1
    };
    let (mut joins, mut runs_on) = (vec![false; labels(&parts)], vec![false; labels(&parts)]);
    for (x, y, part) in parts.enumerate_pixels() {
        let part = part.0[0] as usize;
        if part == 0 {
            continue;
        }
        for (dx, dy) in (-1i64..=1).flat_map(|dx| (-1i64..=1).map(move |dy| (dx, dy))) {
            let (nx, ny) = (i64::from(x) + dx, i64::from(y) + dy);
            if !(0..i64::from(width)).contains(&nx) || !(0..i64::from(height)).contains(&ny) {
                runs_on[part] = true;
                continue;
            }
            let (nx, ny) = (nx as u32, ny as u32);
            if near.get_pixel(nx, ny).0[0] == 0 {
                runs_on[part] = true;
            } else if inside(nx, ny) && is_ink(nx, ny) {
                joins[part] = true;
            }
        }
    }
    // On plain paper every glyph-sized ink part wholly inside the box is
    // lettering: RF-DETR's mask can miss a thin glyph, such as 一, whole. So
    // is one in line with the box, past its ends, when the paper there is
    // plain too and the part stands on the box's own paper: the box can miss
    // it as well, but not past a balloon's or panel's border, which parts the
    // paper. The paper is plain when nearly all of it away from the ink's
    // edges is near its median colour; on art no part counts.
    let all_ink = GrayImage::from_fn(width, height, |x, y| {
        Luma([if is_ink(x, y) { 255 } else { 0 }])
    });
    let pieces = connected_components(&all_ink, Connectivity::Eight, Luma([0]));
    let mut lettering = vec![false; labels(&pieces)];
    let within = |[box_left, box_top, box_right, box_bottom]: [u32; 4], x: u32, y: u32| {
        (box_left..box_right).contains(&(x + left)) && (box_top..box_bottom).contains(&(y + top))
    };
    let edges = dilate(&all_ink, Norm::LInf, 2);
    let plain = |region: [u32; 4]| {
        let paper = |x: u32, y: u32| within(region, x, y) && edges.get_pixel(x, y).0[0] == 0;
        let mut histograms = [[0usize; 256]; 3];
        let mut count = 0;
        for (x, y, pixel) in crop.enumerate_pixels() {
            if paper(x, y) {
                count += 1;
                for (histogram, value) in histograms.iter_mut().zip(pixel.0) {
                    histogram[usize::from(value)] += 1;
                }
            }
        }
        count >= 16 && {
            // Each channel's value at the middle of its sorted values.
            let middle = count / 2;
            let medians = histograms.map(|histogram| {
                let mut seen = 0;
                (0..=u8::MAX)
                    .find(|&value| {
                        seen += histogram[usize::from(value)];
                        seen > middle
                    })
                    .expect("the middle value is counted")
            });
            let close = crop
                .enumerate_pixels()
                .filter(|&(x, y, pixel)| {
                    paper(x, y)
                        && (0..3).all(|channel| {
                            pixel.0[channel].abs_diff(medians[channel]) <= PAPER_TOLERANCE
                        })
                })
                .count();
            close as f32 >= PLAIN_PAPER * count as f32
        }
    };
    let (plain_box, plain_line) = (plain(boxed), plain(lined));
    if plain_box || plain_line {
        // A glyph is no longer than its line is wide: a longer part, such
        // as a ruled line under the text, is art.
        let line = box_right
            .saturating_sub(box_left)
            .min(box_bottom.saturating_sub(box_top));
        // The paper's parts, and the one covering most of the box.
        let paper = connected_components(&all_ink, Connectivity::Four, Luma([255]));
        let mut covering = vec![0usize; labels(&paper)];
        for (x, y, part) in paper.enumerate_pixels() {
            if within(boxed, x, y) {
                covering[part.0[0] as usize] += 1;
            }
        }
        let own = (1..covering.len()).max_by_key(|&part| covering[part]);
        let mut extents: Vec<Option<[u32; 4]>> = vec![None; lettering.len()];
        let (mut leaves_box, mut leaves_line, mut on_own) = (
            vec![false; lettering.len()],
            vec![false; lettering.len()],
            vec![false; lettering.len()],
        );
        for (x, y, piece) in pieces.enumerate_pixels() {
            let piece = piece.0[0] as usize;
            if piece == 0 {
                continue;
            }
            if !within(boxed, x, y) {
                leaves_box[piece] = true;
            }
            if !within(lined, x, y) {
                leaves_line[piece] = true;
            }
            let neighbours = [
                x.checked_sub(1).map(|x| (x, y)),
                (x + 1 < width).then_some((x + 1, y)),
                y.checked_sub(1).map(|y| (x, y)),
                (y + 1 < height).then_some((x, y + 1)),
            ];
            if neighbours
                .into_iter()
                .flatten()
                .any(|(x, y)| Some(paper.get_pixel(x, y).0[0] as usize) == own)
            {
                on_own[piece] = true;
            }
            let extent = extents[piece].get_or_insert([x, y, x, y]);
            *extent = [
                extent[0].min(x),
                extent[1].min(y),
                extent[2].max(x),
                extent[3].max(y),
            ];
        }
        for (piece, extent) in extents.into_iter().enumerate() {
            if let Some([x0, y0, x1, y1]) = extent {
                lettering[piece] = (x1 - x0 + 1).max(y1 - y0 + 1) <= line
                    && ((plain_box && !leaves_box[piece])
                        || (plain_line && !leaves_line[piece] && on_own[piece]));
            }
        }
    }
    let ink = GrayImage::from_fn(width, height, |x, y| {
        let part = parts.get_pixel(x, y).0[0] as usize;
        let cut_off = joins[part] && !runs_on[part];
        Luma([
            if (inside(x, y) && is_ink(x, y))
                || cut_off
                || lettering[pieces.get_pixel(x, y).0[0] as usize]
            {
                255
            } else {
                0
            },
        ])
    });
    ink.pixels()
        .any(|pixel| pixel.0[0] != 0)
        .then_some((ink, [left, top]))
}

/// How far from the paper's median colour, in every channel, a pixel of
/// plain paper lies, and how much of it must.
const PAPER_TOLERANCE: u8 = 32;
const PLAIN_PAPER: f32 = 0.99;

/// `per_1024` pixels for every 1024 of the page's longer side.
fn scaled(per_1024: f32, page_width: u32, page_height: u32) -> u32 {
    ((page_width.max(page_height) as f32 / 1024.0) * per_1024)
        .round()
        .clamp(1.0, 127.0) as u32
}

/// The covered part of `canvas`, placed at (`left`, `top`) on a page, cut to
/// the page.
fn crop(
    canvas: &GrayImage,
    left: i64,
    top: i64,
    page_width: u32,
    page_height: u32,
) -> KoharuLayoutMask {
    let on_page = |x: u32, y: u32| {
        let (page_x, page_y) = (left + i64::from(x), top + i64::from(y));
        (0..i64::from(page_width)).contains(&page_x)
            && (0..i64::from(page_height)).contains(&page_y)
    };
    let covered: Vec<(u32, u32)> = canvas
        .enumerate_pixels()
        .filter(|&(x, y, pixel)| pixel.0[0] != 0 && on_page(x, y))
        .map(|(x, y, _)| (x, y))
        .collect();
    let Some(min_x) = covered.iter().map(|&(x, _)| x).min() else {
        return KoharuLayoutMask {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            pixels: Vec::new(),
        };
    };
    let min_y = covered.iter().map(|&(_, y)| y).min().unwrap_or(0);
    let max_x = covered.iter().map(|&(x, _)| x).max().unwrap_or(0);
    let max_y = covered.iter().map(|&(_, y)| y).max().unwrap_or(0);
    let (width, height) = (max_x + 1 - min_x, max_y + 1 - min_y);
    let mut pixels = vec![0u8; (width * height) as usize];
    for &(x, y) in &covered {
        pixels[((y - min_y) * width + x - min_x) as usize] = 255;
    }
    KoharuLayoutMask {
        x: (left + i64::from(min_x)) as u32,
        y: (top + i64::from(min_y)) as u32,
        width,
        height,
        pixels,
    }
}

#[cfg(test)]
mod tests {
    use image::Rgb;

    use super::*;

    #[test]
    fn a_glyph_past_the_box_counts_only_on_the_box_paper() {
        // A column of glyphs on white paper, a glyph the box cut off below
        // it, a border across the page, and a glyph past the border in line
        // with the column, as a copyright line under a panel lies.
        let mut page = RgbImage::from_pixel(200, 300, Rgb([255, 255, 255]));
        let mut square = |left: u32, top: u32, width: u32, height: u32| {
            for y in top..top + height {
                for x in left..left + width {
                    page.put_pixel(x, y, Rgb([0, 0, 0]));
                }
            }
        };
        let glyphs = [40, 64, 88, 112, 136, 160];
        for top in glyphs {
            square(92, top, 16, 16);
        }
        square(92, 184, 16, 16);
        square(0, 203, 200, 3);
        square(92, 209, 16, 10);
        let mut pixels = vec![0; 16 * 136];
        for top in glyphs {
            for y in top - 40..top - 40 + 16 {
                pixels[(y * 16) as usize..(y * 16 + 16) as usize].fill(255);
            }
        }
        let detected = KoharuLayoutMask {
            x: 92,
            y: 40,
            width: 16,
            height: 136,
            pixels,
        };
        let fitted = glyph_mask(&detected, [80.0, 36.0, 120.0, 180.0], &page);
        assert!(fitted.contains(100, 192));
        assert!(!fitted.contains(100, 213));
    }

    #[test]
    fn a_mask_part_away_from_the_box_is_dropped() {
        // A column of glyphs under a border, and above the border a glyph of
        // another lettering that the detection's mask took in too.
        let mut page = RgbImage::from_pixel(200, 300, Rgb([255, 255, 255]));
        let mut square = |left: u32, top: u32, width: u32, height: u32| {
            for y in top..top + height {
                for x in left..left + width {
                    page.put_pixel(x, y, Rgb([0, 0, 0]));
                }
            }
        };
        for top in [40, 64, 88, 112, 136, 160] {
            square(92, top, 16, 16);
        }
        square(0, 30, 200, 3);
        square(92, 8, 16, 16);
        let pixels = (0..176)
            .flat_map(|y| (0..16).map(move |_| y))
            .map(
                |y| match (8..24).contains(&y) || (40..176).contains(&y) && (y - 40) % 24 < 16 {
                    true => 255,
                    false => 0,
                },
            )
            .collect();
        let detected = KoharuLayoutMask {
            x: 92,
            y: 0,
            width: 16,
            height: 176,
            pixels,
        };
        let fitted = glyph_mask(&detected, [80.0, 36.0, 120.0, 180.0], &page);
        assert!(fitted.contains(100, 48));
        assert!(!fitted.contains(100, 16));
    }

    #[test]
    fn a_fitted_glyph_mask_keeps_its_page_extent() {
        // Two glyphs on white paper, and the paper between them, which the
        // block's closing brings inside the mask.
        let mut page = RgbImage::from_pixel(64, 64, Rgb([255, 255, 255]));
        for top in 20..28 {
            for x in 20..28 {
                page.put_pixel(x, top, Rgb([0, 0, 0]));
                page.put_pixel(x, top + 16, Rgb([0, 0, 0]));
            }
        }
        let mut pixels = vec![0; 8 * 24];
        for y in 0..8 {
            for x in 0..8 {
                pixels[y * 8 + x] = 255;
                pixels[(y + 16) * 8 + x] = 255;
            }
        }
        let detected = KoharuLayoutMask {
            x: 20,
            y: 20,
            width: 8,
            height: 24,
            pixels,
        };

        let fitted = glyph_mask(&detected, [18.0, 18.0, 30.0, 46.0], &page);

        assert!(fitted.contains(24, 24));
        assert!(fitted.contains(24, 40));
        // The counters the block closed over are inked, and the gap between
        // the two glyphs' rows stays paper.
        assert!(!fitted.contains(24, 32));
        assert!(fitted.width > 0 && fitted.height > 0);
        assert!((fitted.x..fitted.x + fitted.width).contains(&24));
        assert!((fitted.y..fitted.y + fitted.height).contains(&40));
    }

    #[test]
    fn an_empty_mask_fits_to_nothing() {
        let page = RgbImage::from_pixel(32, 32, Rgb([255, 255, 255]));
        let empty = KoharuLayoutMask {
            x: 4,
            y: 4,
            width: 8,
            height: 8,
            pixels: vec![0; 64],
        };

        let fitted = glyph_mask(&empty, [4.0, 4.0, 12.0, 12.0], &page);

        assert_eq!(fitted.width, 0);
        assert_eq!(fitted.height, 0);
        assert!(fitted.pixels.is_empty());
    }

    #[test]
    fn a_single_pixel_mask_fits_to_a_glyph() {
        let mut page = RgbImage::from_pixel(32, 32, Rgb([255, 255, 255]));
        page.put_pixel(10, 10, Rgb([0, 0, 0]));
        let single = KoharuLayoutMask {
            x: 10,
            y: 10,
            width: 1,
            height: 1,
            pixels: vec![255],
        };

        let fitted = glyph_mask(&single, [10.0, 10.0, 11.0, 11.0], &page);

        assert!(fitted.width > 0 && fitted.height > 0);
        assert!(fitted.contains(10, 10));
    }
}
