//! 等价性测试(验收标准第 5 条):修复后 `rasterize` 输出与修复前 golden
//! 基线逐像素比对。
//!
//! 基线 `tests/golden/*.png` 由基线取证员用修复前代码生成(构造参数见
//! `tests/golden/manifest.md`),本文件**只读取比对,绝不写入**;场景构造
//! 与 `golden_capture.rs` 采集时完全一致。
//!
//! 容差策略:
//! - G1/G2(1:1 图层,无字形/边缘 AA):每通道差 ≤1,超差即失败;
//! - G4(文字页):背景像素严格 ≤1,字形/抗锯齿像素允许 ≤8 的容差,
//!   并断言近黑文字像素数量与基线相当(防文字整层丢失/偏移);
//! - G3(缩放嵌入图):重采采样滤波不同必然有差,量化差异分布(最大差、
//!   平均差)写入测试输出,断言矩形外背景严格一致、矩形内无结构性丢失。

use std::{collections::BTreeMap, io::Cursor, path::PathBuf, sync::Arc};

use koharu_renderer::{RasterOptions, Renderer};
use koharu_scene::{
    AssetInput, AssetMetadata, AssetRole, At, Authored, Geometry, Origin, PageDraft, RasterLayer,
    RasterLayerKind, Session, SourceText, TextAlignment, TextLayout as SceneTextLayout,
    TextLayoutKind, Translation, Typography, Visibility, WritingMode,
};

fn png_bytes(width: u32, height: u32, color: [u8; 4]) -> Arc<[u8]> {
    encode_png(image::RgbaImage::from_pixel(
        width,
        height,
        image::Rgba(color),
    ))
}

fn encode_png(image: image::RgbaImage) -> Arc<[u8]> {
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner().into()
}

fn asset(bytes: Arc<[u8]>, width: u32, height: u32) -> AssetInput {
    AssetInput::new(
        bytes,
        "image/png",
        AssetMetadata {
            width: Some(width),
            height: Some(height),
            attributes: BTreeMap::new(),
        },
    )
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
}

/// Loads a baseline PNG **read-only**; the golden directory is never written.
fn load_golden(name: &str) -> image::RgbaImage {
    let path = golden_dir().join(format!("{name}.png"));
    image::open(&path)
        .unwrap_or_else(|error| panic!("cannot read golden baseline {}: {error}", path.display()))
        .to_rgba8()
}

/// Renders and rasterizes with a fresh Renderer, mirroring how the golden
/// baselines were captured (same machine, software wgpu).
async fn render_current(session: &Session, page: koharu_scene::EntityId) -> image::RgbaImage {
    let snapshot = session.snapshot();
    let renderer = Renderer::new().unwrap();
    let frame = renderer.render(&snapshot, page).await.unwrap();
    renderer
        .rasterize(&frame, RasterOptions::default())
        .await
        .unwrap()
        .image
}

/// Full-page source + full-page translucent Cleanup layer with one opaque dot
/// at (10, 10). Shared builder for G1/G2/G4, identical to golden_capture.
async fn build_source_with_cleanup(
    width: u32,
    height: u32,
    with_visibility: bool,
) -> (Session, koharu_scene::EntityId) {
    let source_role = AssetRole::new("source").unwrap();
    let mut cleanup = image::RgbaImage::from_pixel(width, height, image::Rgba([240, 30, 20, 128]));
    cleanup.put_pixel(10, 10, image::Rgba([200, 100, 50, 255]));
    let mut session = Session::memory().await.unwrap();
    let mut page = None;
    let patch = session
        .snapshot()
        .patch(|edit| {
            let created = edit.add_page(
                PageDraft::new("page", f64::from(width), f64::from(height)),
                At::End,
            )?;
            edit.set_asset(
                created,
                &source_role,
                asset(png_bytes(width, height, [10, 20, 200, 255]), width, height),
            )?;
            let cleanup_layer = edit.add_entity(created, At::Start)?;
            edit.set(
                cleanup_layer,
                &RasterLayer {
                    origin: Origin::User,
                    name: "Cleanup".to_owned(),
                    kind: RasterLayerKind::Cleanup,
                },
            )?;
            edit.set_asset(
                cleanup_layer,
                &source_role,
                asset(encode_png(cleanup), width, height),
            )?;
            if with_visibility {
                edit.set(
                    cleanup_layer,
                    &Visibility {
                        origin: Origin::User,
                        visible: true,
                        opacity: 0.5,
                    },
                )?;
            }
            page = Some(created);
            Ok(())
        })
        .unwrap();
    let _ = session.commit(patch).await.unwrap();
    (session, page.unwrap())
}

/// Max per-channel absolute difference between two equally sized pixels.
fn pixel_diff(a: [u8; 4], b: [u8; 4]) -> u8 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

/// Straight-alpha src-over used to derive expected background composites.
fn src_over(dst: [u8; 4], src: [u8; 4]) -> [u8; 4] {
    let sa = f64::from(src[3]) / 255.0;
    let da = f64::from(dst[3]) / 255.0;
    let out_a = sa + da * (1.0 - sa);
    if out_a <= 0.0 {
        return [0, 0, 0, 0];
    }
    let channel =
        |s: u8, d: u8| ((f64::from(s) * sa + f64::from(d) * da * (1.0 - sa)) / out_a).round() as u8;
    [
        channel(src[0], dst[0]),
        channel(src[1], dst[1]),
        channel(src[2], dst[2]),
        (out_a * 255.0).round() as u8,
    ]
}

/// Full-image comparison against a baseline; returns (max diff, worst pixel,
/// number of pixels exceeding `tolerance`).
fn compare_full(
    current: &image::RgbaImage,
    golden: &image::RgbaImage,
    tolerance: u8,
) -> (u8, (u32, u32), usize) {
    assert_eq!(
        current.dimensions(),
        golden.dimensions(),
        "output size must match the golden baseline"
    );
    let mut max = 0_u8;
    let mut worst = (0, 0);
    let mut over = 0_usize;
    for (x, y, pixel) in current.enumerate_pixels() {
        let diff = pixel_diff(pixel.0, golden.get_pixel(x, y).0);
        if diff > max {
            max = diff;
            worst = (x, y);
        }
        if diff > tolerance {
            over += 1;
        }
    }
    (max, worst, over)
}

fn assert_near(actual: [u8; 4], expected: [u8; 4], tolerance: u8, label: &str) {
    let diff = pixel_diff(actual, expected);
    assert!(
        diff <= tolerance,
        "{label}: got {actual:?}, expected {expected:?} (max channel diff {diff} > {tolerance})"
    );
}

/// G1: 1:1 source + translucent Cleanup, flat colors only — every pixel must
/// match the pre-fix baseline within 1 per channel.
#[tokio::test]
async fn g1_source_cleanup_matches_golden_within_one() {
    let (session, page) = build_source_with_cleanup(64, 96, false).await;
    let current = render_current(&session, page).await;
    let golden = load_golden("g1_source_cleanup_1x1");
    let (max, worst, over) = compare_full(&current, &golden, 1);
    println!("G1 diff vs golden: max={max}, pixels_over_1={over}, worst_pixel={worst:?}");
    assert_eq!(
        over, 0,
        "G1 has {over} pixels beyond per-channel diff 1 (max {max} at {worst:?})"
    );
    // manifest.md 抽样点。
    assert_near(
        current.get_pixel(0, 0).0,
        [125, 25, 110, 255],
        1,
        "G1 (0,0)",
    );
    assert_near(
        current.get_pixel(10, 10).0,
        [200, 100, 50, 255],
        1,
        "G1 (10,10)",
    );
    assert_near(
        current.get_pixel(63, 95).0,
        [125, 25, 110, 255],
        1,
        "G1 (63,95)",
    );
}

/// G2: same as G1 but the Cleanup layer carries Visibility { opacity: 0.5 }.
#[tokio::test]
async fn g2_cleanup_opacity_matches_golden_within_one() {
    let (session, page) = build_source_with_cleanup(64, 96, true).await;
    let current = render_current(&session, page).await;
    let golden = load_golden("g2_cleanup_opacity");
    let (max, worst, over) = compare_full(&current, &golden, 1);
    println!("G2 diff vs golden: max={max}, pixels_over_1={over}, worst_pixel={worst:?}");
    assert_eq!(
        over, 0,
        "G2 has {over} pixels beyond per-channel diff 1 (max {max} at {worst:?})"
    );
    // manifest.md 抽样点。
    assert_near(current.get_pixel(0, 0).0, [68, 23, 155, 255], 1, "G2 (0,0)");
    assert_near(
        current.get_pixel(10, 10).0,
        [105, 60, 125, 255],
        1,
        "G2 (10,10)",
    );
    assert_near(
        current.get_pixel(63, 95).0,
        [68, 23, 155, 255],
        1,
        "G2 (63,95)",
    );
}

/// G3: 16x16 gradient stretched into a 64x64 rectangle over a checkerboard.
/// The CPU path samples nearest-neighbour while the pre-fix vello path used
/// its own resampling filter, so interior pixels legitimately differ. Assert
/// structure: checkerboard background outside the rectangle is pixel-exact,
/// the embedded gradient is present inside, and the diff distribution shows
/// no gross artifacts (distribution is printed for the report).
#[tokio::test]
async fn g3_embedded_scaled_matches_golden_structurally() {
    let source_role = AssetRole::new("source").unwrap();
    let mut checker = image::RgbaImage::new(128, 128);
    for (x, y, pixel) in checker.enumerate_pixels_mut() {
        let dark = ((x / 32) + (y / 32)) % 2 == 0;
        *pixel = image::Rgba(if dark {
            [20, 20, 20, 255]
        } else {
            [235, 235, 235, 255]
        });
    }
    let mut gradient = image::RgbaImage::new(16, 16);
    for (x, y, pixel) in gradient.enumerate_pixels_mut() {
        *pixel = image::Rgba([(x as u8) * 16, (y as u8) * 16, 128, 255]);
    }
    let mut session = Session::memory().await.unwrap();
    let mut page = None;
    let patch = session
        .snapshot()
        .patch(|edit| {
            let created = edit.add_page(PageDraft::new("page", 128.0, 128.0), At::End)?;
            edit.set_asset(created, &source_role, asset(encode_png(checker), 128, 128))?;
            let embedded = edit.add_entity(created, At::End)?;
            edit.set(embedded, &Geometry::rectangle(32.0, 32.0, 64.0, 64.0))?;
            edit.set_asset(embedded, &source_role, asset(encode_png(gradient), 16, 16))?;
            page = Some(created);
            Ok(())
        })
        .unwrap();
    let _ = session.commit(patch).await.unwrap();
    let current = render_current(&session, page.unwrap()).await;
    let golden = load_golden("g3_embedded_scaled");
    assert_eq!(current.dimensions(), (128, 128));

    // 矩形外(背景棋盘格):严格 ≤1 —— 背景不得丢失或被污染。
    let mut out_max = 0_u8;
    let mut out_over = 0_usize;
    let mut out_worst = (0, 0);
    // 矩形内(嵌入渐变):量化重采样差异分布。
    let mut in_max = 0_u8;
    let mut in_sum = 0_u64;
    let mut in_over_32 = 0_usize;
    let mut in_worst = (0, 0);
    let mut in_count = 0_usize;
    let mut all_sum = 0_u64;
    let mut all_max = 0_u8;
    let mut all_worst = (0, 0);
    for y in 0..128_u32 {
        for x in 0..128_u32 {
            let diff = pixel_diff(current.get_pixel(x, y).0, golden.get_pixel(x, y).0);
            all_sum += u64::from(diff);
            if diff > all_max {
                all_max = diff;
                all_worst = (x, y);
            }
            let inside = (32..96).contains(&x) && (32..96).contains(&y);
            if inside {
                in_count += 1;
                in_sum += u64::from(diff);
                if diff > in_max {
                    in_max = diff;
                    in_worst = (x, y);
                }
                if diff > 32 {
                    in_over_32 += 1;
                }
            } else {
                if diff > out_max {
                    out_max = diff;
                    out_worst = (x, y);
                }
                if diff > 1 {
                    out_over += 1;
                }
            }
        }
    }
    let in_mean = f64::from(in_sum as u32) / f64::from(in_count as u32);
    let all_mean = f64::from(all_sum as u32) / f64::from(128 * 128);
    println!(
        "G3 diff vs golden — outside rect: max={out_max} at {out_worst:?}, over_1={out_over}; \
         inside rect: max={in_max} at {in_worst:?}, mean={in_mean:.3}, pixels>32={in_over_32}/{in_count}; \
         overall: max={all_max} at {all_worst:?}, mean={all_mean:.3}"
    );
    println!(
        "G3 embedded samples — current (33,33)={:?}, (64,64)={:?}, (95,95)={:?}; golden (33,33)={:?}, (64,64)={:?}, (95,95)={:?}",
        current.get_pixel(33, 33).0,
        current.get_pixel(64, 64).0,
        current.get_pixel(95, 95).0,
        golden.get_pixel(33, 33).0,
        golden.get_pixel(64, 64).0,
        golden.get_pixel(95, 95).0,
    );

    // 结构 1:矩形外背景无任何丢失/伪影。
    assert_eq!(
        out_over, 0,
        "G3: {out_over} background pixels differ by more than 1 (max {out_max} at {out_worst:?})"
    );
    // 结构 2:manifest 抽样点(嵌入渐变内)与基线一致(允许 ≤16 滤波差)。
    for (x, y) in [(33, 33), (64, 64), (95, 95)] {
        let diff = pixel_diff(current.get_pixel(x, y).0, golden.get_pixel(x, y).0);
        assert!(
            diff <= 16,
            "G3: embedded gradient sample ({x},{y}) differs too much: current {:?} vs golden {:?} (diff {diff})",
            current.get_pixel(x, y).0,
            golden.get_pixel(x, y).0
        );
    }
    // 结构 3:矩形内整体差异受控 —— 无整层丢失(那会是均值上百的棋盘格
    // 差异)、无明显伪影。
    assert!(
        in_mean <= 20.0,
        "G3: mean in-rect diff {in_mean:.3} exceeds 20 — embedded layer may be lost or corrupted"
    );
    assert!(
        in_max <= 64,
        "G3: max in-rect diff {in_max} at {in_worst:?} exceeds 64 — visible artifact"
    );
}

/// G4: G1's images plus a Chinese text layer. Background pixels must be
/// pixel-exact; glyph/anti-aliased pixels get a small tolerance; the dark
/// stroke pixel count must stay comparable to the baseline so text cannot
/// silently vanish or shift.
#[tokio::test]
async fn g4_text_over_images_matches_golden() {
    let (mut session, page) = build_source_with_cleanup(64, 96, false).await;
    let patch = session
        .snapshot()
        .patch(|edit| {
            let content = edit.add_text_content(page, At::End)?;
            edit.set(
                content,
                &SourceText {
                    text: Authored::user("原文".to_owned()),
                    language: None,
                },
            )?;
            edit.set(
                content,
                &Translation {
                    text: Authored::user("你好世界".to_owned()),
                    language: None,
                },
            )?;
            let text = edit.add_text_layer(
                page,
                At::End,
                content,
                &SceneTextLayout {
                    origin: Origin::User,
                    kind: TextLayoutKind::Paragraph,
                    angle_degrees: None,
                },
            )?;
            edit.set(text, &Geometry::rectangle(8.0, 8.0, 48.0, 40.0))?;
            edit.set(
                text,
                &Typography {
                    origin: Origin::User,
                    preferred_font: None,
                    font_weight: None,
                    font_style: None,
                    size: Some(20.0),
                    auto_fit: false,
                    color: Some([0, 0, 0, 255]),
                    stroke_color: None,
                    stroke_width: None,
                    alignment: Some(TextAlignment::Start),
                    writing_mode: Some(WritingMode::Horizontal),
                    extensions: BTreeMap::new(),
                },
            )?;
            Ok(())
        })
        .unwrap();
    let _ = session.commit(patch).await.unwrap();
    let current = render_current(&session, page).await;
    let golden = load_golden("g4_text_over_images");
    assert_eq!(current.dimensions(), golden.dimensions());

    let background = src_over([10, 20, 200, 255], [240, 30, 20, 128]);
    assert_eq!(
        background,
        [125, 25, 110, 255],
        "manifest background composite"
    );
    let dot = [200_u8, 100, 50, 255];
    const GLYPH_TOLERANCE: u8 = 8;

    let mut flat_max = 0_u8;
    let mut flat_worst = (0, 0);
    let mut flat_over = 0_usize;
    let mut glyph_max = 0_u8;
    let mut glyph_worst = (0, 0);
    let mut glyph_over = 0_usize;
    let mut golden_dark = 0_usize;
    let mut current_dark = 0_usize;
    let mut golden_nonbg = 0_usize;
    let mut current_nonbg = 0_usize;
    for (x, y, pixel) in current.enumerate_pixels() {
        let current_px = pixel.0;
        let golden_px = golden.get_pixel(x, y).0;
        let diff = pixel_diff(current_px, golden_px);
        let golden_flat = pixel_diff(golden_px, background) <= 1 || pixel_diff(golden_px, dot) <= 1;
        let current_flat =
            pixel_diff(current_px, background) <= 1 || pixel_diff(current_px, dot) <= 1;
        if golden_flat && current_flat {
            if diff > 1 && diff > flat_max {
                flat_max = diff;
                flat_worst = (x, y);
            }
            if diff > 1 {
                flat_over += 1;
            }
        } else {
            if diff > glyph_max {
                glyph_max = diff;
                glyph_worst = (x, y);
            }
            if diff > GLYPH_TOLERANCE {
                glyph_over += 1;
            }
        }
        if u32::from(golden_px[0]) < 40
            && u32::from(golden_px[1]) < 40
            && u32::from(golden_px[2]) < 40
        {
            golden_dark += 1;
        }
        if u32::from(current_px[0]) < 40
            && u32::from(current_px[1]) < 40
            && u32::from(current_px[2]) < 40
        {
            current_dark += 1;
        }
        if pixel_diff(golden_px, background) > 1 {
            golden_nonbg += 1;
        }
        if pixel_diff(current_px, background) > 1 {
            current_nonbg += 1;
        }
    }
    println!(
        "G4 diff vs golden — flat_violations={flat_over} (max {flat_max} at {flat_worst:?}); \
         glyph max={glyph_max} at {glyph_worst:?}, violations>{GLYPH_TOLERANCE}={glyph_over}; \
         near-black pixels golden={golden_dark} current={current_dark}; \
         non-background pixels golden={golden_nonbg} current={current_nonbg}"
    );
    assert_eq!(
        flat_over, 0,
        "G4: {flat_over} background pixels differ by more than 1 (max {flat_max} at {flat_worst:?})"
    );
    assert_eq!(
        glyph_over, 0,
        "G4: {glyph_over} glyph/AA pixels differ by more than {GLYPH_TOLERANCE} \
         (max {glyph_max} at {glyph_worst:?})"
    );
    // 文字结构:近黑笔画像素数量与基线相当(±20%),防止文字整层丢失。
    assert!(
        current_dark * 5 >= golden_dark * 4 && current_dark * 5 <= golden_dark * 6,
        "G4: near-black stroke pixel count drifted too far: golden={golden_dark}, current={current_dark}"
    );
    // manifest.md 抽样点。
    assert_near(
        current.get_pixel(0, 0).0,
        [125, 25, 110, 255],
        1,
        "G4 (0,0) background",
    );
    assert_near(
        current.get_pixel(10, 10).0,
        [200, 100, 50, 255],
        1,
        "G4 (10,10) dot",
    );
    assert_near(
        current.get_pixel(63, 95).0,
        [125, 25, 110, 255],
        1,
        "G4 (63,95) background",
    );
    let (glyph_x, glyph_y) = (18, 7);
    let glyph_diff = pixel_diff(
        current.get_pixel(glyph_x, glyph_y).0,
        golden.get_pixel(glyph_x, glyph_y).0,
    );
    assert!(
        glyph_diff <= GLYPH_TOLERANCE,
        "G4 glyph sample (18,7): current {:?} vs golden {:?} (diff {glyph_diff})",
        current.get_pixel(glyph_x, glyph_y).0,
        golden.get_pixel(glyph_x, glyph_y).0
    );
}
