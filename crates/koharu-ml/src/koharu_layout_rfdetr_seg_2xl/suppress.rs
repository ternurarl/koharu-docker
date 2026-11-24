//! Duplicates of one object that separate queries predict, removed after
//! the network as RF-DETR's maintainers advise (roboflow/rf-detr#187 and
//! #434): its one-to-one matching only teaches the model to avoid them.
//!
//! Ported from upstream koharu-rs/koharu 08193a3c's
//! `koharu_layout_highres/suppress.rs`, keeping only `suppress`: `merge` and
//! `disjoint` judge onomatopoeia against its glyphs, which this fork measures
//! in `koharu-pipeline`'s detection stage instead.

use super::processor::KoharuLayoutDetection;

/// A detection overlapping a more confident one of its label by at least
/// this IoU is the same object, as in supervision's `with_nms` default. The
/// checkpoint's training labels hold 116 such pairs among 81,638 panels and
/// hardly any among the other classes.
const IOU: f32 = 0.5;

/// A box with at least this share of it inside another nests in it. Text
/// nesting in text is the same block, a fragment of it, or it merged with an
/// aside: training labels nest text in text only 9 times among 127,678
/// boxes. On 212 pages of Blue Archive's comics, suppressing nested text kept
/// the right box of 11 fragment pairs of 11 and 15 merged pairs of 22, where
/// IoU alone kept 5 of each. Bubbles and panels nest hundreds of times, so
/// nesting alone suppresses only text.
const NESTED: f32 = 0.8;

/// Drops each detection that duplicates a more confident one, keeping the
/// rest in their order. Onomatopoeia are left to the caller, which judges
/// them against their glyphs.
pub(super) fn suppress(detections: &mut Vec<KoharuLayoutDetection>) {
    let mut ranked: Vec<usize> = (0..detections.len()).collect();
    ranked.sort_by(|&left, &right| detections[right].score.total_cmp(&detections[left].score));
    let mut kept = vec![false; detections.len()];
    for candidate in ranked {
        kept[candidate] = !detections
            .iter()
            .zip(&kept)
            .any(|(other, &kept)| kept && duplicate(other, &detections[candidate]));
    }
    let mut kept = kept.into_iter();
    detections.retain(|_| kept.next() == Some(true));
}

fn duplicate(kept: &KoharuLayoutDetection, candidate: &KoharuLayoutDetection) -> bool {
    if kept.label != candidate.label || candidate.label == "onomatopoeia" {
        return false;
    }
    let (iou, nested) = overlap(kept.bbox, candidate.bbox);
    iou >= IOU || candidate.label == "text" && nested >= NESTED
}

/// Two boxes' IoU, and the share of the smaller inside the other.
fn overlap(left: [f32; 4], right: [f32; 4]) -> (f32, f32) {
    let both = (left[2].min(right[2]) - left[0].max(right[0])).max(0.0)
        * (left[3].min(right[3]) - left[1].max(right[1])).max(0.0);
    let (left, right) = (area(left), area(right));
    let ratio = |whole: f32| if whole > 0.0 { both / whole } else { 0.0 };
    (ratio(left + right - both), ratio(left.min(right)))
}

fn area(bbox: [f32; 4]) -> f32 {
    (bbox[2] - bbox[0]).max(0.0) * (bbox[3] - bbox[1]).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::super::processor::KoharuLayoutMask;
    use super::{KoharuLayoutDetection, suppress};

    fn detection(label: &str, score: f32, bbox: [f32; 4]) -> KoharuLayoutDetection {
        KoharuLayoutDetection {
            label_id: 0,
            label: label.to_owned(),
            score,
            bbox,
            area: 0,
            mask: KoharuLayoutMask {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
                pixels: Vec::new(),
            },
        }
    }

    fn labels(detections: &[KoharuLayoutDetection]) -> Vec<(&str, f32)> {
        detections
            .iter()
            .map(|detection| (detection.label.as_str(), detection.score))
            .collect()
    }

    #[test]
    fn duplicates_of_a_class_are_removed_in_their_order() {
        let mut detections = vec![
            detection("text", 0.8, [5.0, 5.0, 105.0, 105.0]),
            detection("bubble", 0.7, [0.0, 0.0, 100.0, 100.0]),
            detection("text", 0.9, [0.0, 0.0, 100.0, 100.0]),
            detection("text", 0.5, [20.0, 20.0, 80.0, 80.0]),
            detection("text", 0.6, [200.0, 0.0, 250.0, 50.0]),
        ];

        suppress(&mut detections);

        assert_eq!(
            labels(&detections),
            [("bubble", 0.7), ("text", 0.9), ("text", 0.6)]
        );
    }

    #[test]
    fn a_text_fragment_nested_in_the_whole_block_is_removed() {
        // The fragment is wholly inside the block, so only nesting separates
        // them: their IoU is 0.3, under IOU.
        let mut detections = vec![
            detection("text", 0.95, [0.0, 0.0, 100.0, 100.0]),
            detection("text", 0.6, [20.0, 20.0, 80.0, 70.0]),
        ];

        suppress(&mut detections);

        assert_eq!(labels(&detections), [("text", 0.95)]);
    }

    #[test]
    fn nested_bubbles_stay_and_nearly_identical_panels_are_removed() {
        // The bubble's IoU with its surround is 0.36, so only text is
        // deduplicated by nesting; the panel's is 0.87, so it goes.
        let mut detections = vec![
            detection("bubble", 0.9, [0.0, 0.0, 100.0, 100.0]),
            detection("bubble", 0.8, [0.0, 0.0, 60.0, 60.0]),
            detection("panel", 0.9, [0.0, 0.0, 300.0, 300.0]),
            detection("panel", 0.8, [10.0, 10.0, 290.0, 290.0]),
        ];

        suppress(&mut detections);

        assert_eq!(
            labels(&detections),
            [("bubble", 0.9), ("bubble", 0.8), ("panel", 0.9)]
        );
    }

    #[test]
    fn overlapping_onomatopoeia_are_left_to_the_glyph_judgement() {
        let mut detections = vec![
            detection("onomatopoeia", 0.9, [0.0, 0.0, 100.0, 100.0]),
            detection("onomatopoeia", 0.8, [0.0, 0.0, 100.0, 100.0]),
        ];

        suppress(&mut detections);

        assert_eq!(
            labels(&detections),
            [("onomatopoeia", 0.9), ("onomatopoeia", 0.8)]
        );
    }

    #[test]
    fn a_text_and_a_bubble_on_one_box_stay() {
        let mut detections = vec![
            detection("text", 0.9, [0.0, 0.0, 100.0, 100.0]),
            detection("bubble", 0.8, [0.0, 0.0, 100.0, 100.0]),
        ];

        suppress(&mut detections);

        assert_eq!(labels(&detections), [("text", 0.9), ("bubble", 0.8)]);
    }
}
