//! Per-page input resolution for the RF-DETR layout model.
//!
//! The checkpoint is square at `model::CHECKPOINT_RESOLUTION`, and so is the
//! positional embedding table registered for it. This module owns the single
//! mapping from one page's size to the `(height, width)` the network runs at:
//! `KOHARU_LAYOUT_RESOLUTION` picks the policy, and a `Budget` policy lets a
//! rectangular page keep its aspect ratio instead of being stretched into a
//! square. Both the preprocessor and the mask-shape check in the postprocessor
//! resolve a page through the same pure function, so they cannot disagree.

use anyhow::Result;

use super::model::{CHECKPOINT_RESOLUTION, RESOLUTION_MULTIPLE};

/// The environment variable this policy is read from.
const ENV_VAR: &str = "KOHARU_LAYOUT_RESOLUTION";

/// 1920², the checkpoint's native pixel count.
const HIGHRES_PIXELS: i64 = CHECKPOINT_RESOLUTION * CHECKPOINT_RESOLUTION;

/// 1152².
const BALANCED_PIXELS: i64 = 1152 * 1152;

/// 768×1272, whose square solve is 984².
const FAST_PIXELS: i64 = 768 * 1272;

/// The longest aspect ratio a budget solve keeps. Past 3:1 the short side of a
/// `balanced` solve drops under 400 pixels, where a text glyph is a few tokens
/// wide; stretching the page further spends pixels on width that the detector
/// cannot use.
const MAX_ASPECT: f64 = 3.0;

/// Pages within this ratio of square solve to a square target: the budget
/// already fits a square at full utilization, and a square keeps the whole
/// positional embedding table on its native grid.
const SQUARE_BAND: f64 = 1.1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResolutionPolicy {
    /// No `KOHARU_LAYOUT_RESOLUTION`: the checkpoint's square resolution.
    Checkpoint,
    /// One square side; the page shape is ignored.
    Square(i64),
    /// An explicit size; the page shape is ignored.
    Fixed { height: i64, width: i64 },
    /// A pixel budget, solved against each page's aspect ratio.
    Budget { pixels: i64 },
}

impl ResolutionPolicy {
    /// Reads the policy from the environment. An unset variable keeps the
    /// checkpoint's resolution.
    pub(super) fn from_env() -> Result<Self> {
        match std::env::var(ENV_VAR) {
            Ok(raw) => Self::parse(&raw),
            Err(std::env::VarError::NotPresent) => Ok(Self::Checkpoint),
            Err(error @ std::env::VarError::NotUnicode(_)) => {
                Err(anyhow::Error::new(error).context(format!("{ENV_VAR} is not valid UTF-8")))
            }
        }
    }

    /// Parses one `KOHARU_LAYOUT_RESOLUTION` value. Pure, so the accepted
    /// spellings are tested without a network or a process environment.
    pub(super) fn parse(raw: &str) -> Result<Self> {
        if raw.is_empty() {
            return Ok(Self::Checkpoint);
        }
        let policy = match raw {
            "highres" => Self::Budget {
                pixels: HIGHRES_PIXELS,
            },
            "balanced" => Self::Budget {
                pixels: BALANCED_PIXELS,
            },
            "fast" => Self::Budget {
                pixels: FAST_PIXELS,
            },
            _ => {
                if let Some(pixels) = raw.strip_prefix("auto:") {
                    Self::Budget {
                        pixels: number(raw, pixels)?,
                    }
                } else if let Some((width, height)) = raw.split_once('x') {
                    Self::Fixed {
                        height: number(raw, height)?,
                        width: number(raw, width)?,
                    }
                } else {
                    Self::Square(number(raw, raw)?)
                }
            }
        };
        let on_grid = match policy {
            Self::Checkpoint => true,
            Self::Square(side) => side > 0 && side % RESOLUTION_MULTIPLE == 0,
            Self::Fixed { height, width } => {
                height > 0
                    && width > 0
                    && height % RESOLUTION_MULTIPLE == 0
                    && width % RESOLUTION_MULTIPLE == 0
            }
            Self::Budget { pixels } => pixels > 0,
        };
        if !on_grid {
            return Err(invalid(raw));
        }
        Ok(policy)
    }

    /// The `(height, width)` to run one page at. Pure: the postprocessor calls
    /// it again from the page size alone to know the mask shape it must expect.
    pub(super) fn target_size(&self, page_width: u32, page_height: u32) -> (i64, i64) {
        match *self {
            Self::Checkpoint => (CHECKPOINT_RESOLUTION, CHECKPOINT_RESOLUTION),
            Self::Square(side) => (side, side),
            Self::Fixed { height, width } => (height, width),
            Self::Budget { pixels } => budget_target_size(pixels, page_width, page_height),
        }
    }
}

/// Solves one page against a pixel budget.
///
/// The ideal sides follow from the budget and the page's aspect ratio: `h₀ =
/// √(pixels / aspect)`, `w₀ = √(pixels · aspect)` for `aspect = width / height`.
/// The two branches then differ:
///
/// - A page within `SQUARE_BAND` of square takes a square target, `√pixels`
///   rounded down to `RESOLUTION_MULTIPLE`, which cannot exceed the budget
///   because a square solve already uses all of it.
/// - Otherwise each side goes to the nearest multiple of `RESOLUTION_MULTIPLE`,
///   and the longer side is then shortened one step at a time until the product
///   fits the budget. Rounding down instead would leave percent-level budget
///   unused: `balanced` on a 1200×1987 page solves to 1482.4×895.3 and lands on
///   1488×888, while flooring would give 1464×888.
///
/// The result always satisfies `height, width > 0` and both divide by
/// `RESOLUTION_MULTIPLE`; a budget below `24²` still yields `24×24` because the
/// network cannot run smaller.
fn budget_target_size(pixels: i64, page_width: u32, page_height: u32) -> (i64, i64) {
    let aspect = if page_width == 0 || page_height == 0 {
        1.0
    } else {
        (f64::from(page_width) / f64::from(page_height)).clamp(1.0 / MAX_ASPECT, MAX_ASPECT)
    };
    let budget = pixels as f64;
    let (mut height, mut width) = if (1.0 / SQUARE_BAND..=SQUARE_BAND).contains(&aspect) {
        let side = floor_to_multiple(budget.sqrt());
        (side, side)
    } else {
        (
            round_to_multiple((budget / aspect).sqrt()),
            round_to_multiple((budget * aspect).sqrt()),
        )
    };
    while height * width > pixels {
        // Shortening the longer side keeps the page's shape and gives up the
        // fewest pixels per step.
        if height >= width && height > RESOLUTION_MULTIPLE {
            height -= RESOLUTION_MULTIPLE;
        } else if width > RESOLUTION_MULTIPLE {
            width -= RESOLUTION_MULTIPLE;
        } else {
            break;
        }
    }
    (height, width)
}

fn floor_to_multiple(value: f64) -> i64 {
    ((value / RESOLUTION_MULTIPLE as f64).floor() as i64)
        .max(1)
        .saturating_mul(RESOLUTION_MULTIPLE)
}

fn round_to_multiple(value: f64) -> i64 {
    ((value / RESOLUTION_MULTIPLE as f64).round() as i64)
        .max(1)
        .saturating_mul(RESOLUTION_MULTIPLE)
}

fn number(raw: &str, text: &str) -> Result<i64> {
    text.parse::<i64>().map_err(|_| invalid(raw))
}

fn invalid(raw: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "invalid {ENV_VAR} value {raw:?}: expected `highres`, `balanced`, `fast`, \
         a square side as a multiple of {RESOLUTION_MULTIPLE} (for example 1152), \
         `widthxheight` with both sides multiples of {RESOLUTION_MULTIPLE} \
         (for example 888x1488), `auto:<pixels>` with a positive pixel count, \
         or an unset/empty value for the checkpoint's {CHECKPOINT_RESOLUTION}-pixel square"
    )
}

#[cfg(test)]
mod tests {
    use super::ResolutionPolicy;

    fn policy(raw: &str) -> ResolutionPolicy {
        ResolutionPolicy::parse(raw).expect("value should parse")
    }

    /// 1200×1987 is the portrait page the preset budgets were tuned on; its
    /// 0.604 ratio is the same as the 768×1272 `fast` reference page.
    const PORTRAIT: (u32, u32) = (1200, 1987);

    #[test]
    fn budget_presets_follow_the_page_shape() {
        assert_eq!(
            policy("balanced").target_size(PORTRAIT.0, PORTRAIT.1),
            (1488, 888)
        );
        assert_eq!(
            policy("highres").target_size(PORTRAIT.0, PORTRAIT.1),
            (2472, 1488)
        );
        assert_eq!(policy("fast").target_size(768, 1272), (1272, 768));
        assert_eq!(policy("balanced").target_size(1200, 1200), (1152, 1152));
        assert_eq!(policy("fast").target_size(1200, 1200), (984, 984));
        assert_eq!(
            policy("auto:1327104").target_size(PORTRAIT.0, PORTRAIT.1),
            (1488, 888)
        );
    }

    #[test]
    fn explicit_sizes_ignore_the_page_shape() {
        assert_eq!(
            policy("888x1488").target_size(PORTRAIT.0, PORTRAIT.1),
            (1488, 888)
        );
        assert_eq!(
            policy("1152").target_size(PORTRAIT.0, PORTRAIT.1),
            (1152, 1152)
        );
        assert_eq!(
            ResolutionPolicy::Checkpoint.target_size(PORTRAIT.0, PORTRAIT.1),
            (1920, 1920)
        );
    }

    #[test]
    fn every_target_stays_on_the_window_grid() {
        for policy in [policy("fast"), policy("balanced"), policy("highres")] {
            for (width, height) in [(1200, 1987), (1, 8000), (8000, 1), (0, 0), (1, 1)] {
                let (target_h, target_w) = policy.target_size(width, height);
                assert!(
                    target_h > 0 && target_w > 0,
                    "{policy:?} on {width}×{height}"
                );
                assert_eq!(target_h % 24, 0, "{policy:?} on {width}×{height}");
                assert_eq!(target_w % 24, 0, "{policy:?} on {width}×{height}");
            }
        }
    }

    #[test]
    fn budget_solves_never_exceed_the_budget() {
        for policy in [policy("fast"), policy("balanced"), policy("highres")] {
            for (width, height) in [(1200, 1987), (1, 8000), (8000, 1), (1200, 1200)] {
                let ResolutionPolicy::Budget { pixels } = policy else {
                    unreachable!("only budgets are checked here")
                };
                let (target_h, target_w) = policy.target_size(width, height);
                assert!(
                    target_h * target_w <= pixels,
                    "{policy:?} on {width}×{height} gives {target_h}×{target_w}"
                );
            }
        }
    }

    #[test]
    fn malformed_values_are_rejected() {
        for raw in [
            " ",
            "1000",
            "1x0",
            "auto:0",
            "auto:",
            "1920X1920",
            "0",
            "-24",
            "fast!",
        ] {
            assert!(
                ResolutionPolicy::parse(raw).is_err(),
                "{raw:?} should be rejected"
            );
        }
        // 1080 is 24 × 45, so a multiple of 24 is accepted even though the
        // task's example list called it malformed.
        assert_eq!(policy("1080").target_size(1200, 1200), (1080, 1080));
    }
}
