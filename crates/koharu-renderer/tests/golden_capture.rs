//! Golden raster capture for pre-fix (vello 0.9.0) rendering baselines.
//!
//! Every test here is `#[ignore]`d on purpose: the PNGs under
//! `tests/golden/` are the pixel ground truth produced by the CURRENT,
//! UNMODIFIED renderer (the one with the vello image-atlas drop bug).
//! Re-running them with a fixed renderer would silently overwrite the
//! baseline, so the only sanctioned invocation is:
//!
//! ```text
//! PATH=/tmp/fakebin:$PATH cargo test -p koharu-renderer --test golden_capture -- --ignored
//! ```
//!
//! Do NOT remove `#[ignore]` from these tests.

use std::{collections::BTreeMap, io::Cursor, path::PathBuf, sync::Arc};

use koharu_renderer::{Raster, RasterOptions, Renderer};
use koharu_scene::{
    AssetInput, AssetMetadata, AssetRole, At, Authored, Geometry, Origin, PageDraft, RasterLayer,
    RasterLayerKind, Session, SourceText, TextAlignment, TextLayout as SceneTextLayout,
    TextLayoutKind, Translation, Typography, Visibility, WritingMode,
};

fn png_bytes(width: u32, height: u32, color: [u8; 4]) -> Arc<[u8]> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba(color));
    encode_png(image)
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden")
}

async fn capture(name: &str, session: &mut Session, page: koharu_scene::EntityId) -> Raster {
    let snapshot = session.snapshot();
    let renderer = Renderer::new().unwrap();
    let frame = renderer.render(&snapshot, page).await.unwrap();
    let raster = renderer
        .rasterize(&frame, RasterOptions::default())
        .await
        .unwrap();
    let dir = golden_dir();
    std::fs::create_dir_all(&dir).unwrap();
    raster
        .image
        .save(dir.join(format!("{name}.png")))
        .unwrap();
    raster
}

/// Full-page source + full-page translucent Cleanup layer with one opaque
/// dot at (10, 10). Shared builder for G1/G2/G4.
async fn build_source_with_cleanup(width: u32, height: u32, with_visibility: bool) -> (Session, koharu_scene::EntityId) {
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

/// G1: 64x96 page, 1:1, source [10,20,200,255], Cleanup [240,30,20,128]
/// with an opaque [200,100,50,255] pixel at (10,10).
#[tokio::test]
#[ignore = "golden baseline capture; run only via `cargo test -p koharu-renderer --test golden_capture -- --ignored`"]
async fn g1_source_cleanup_1x1() {
    let (mut session, page) = build_source_with_cleanup(64, 96, false).await;
    capture("g1_source_cleanup_1x1", &mut session, page).await;
}

/// G2: identical to G1 but the Cleanup entity carries
/// `Visibility { visible: true, opacity: 0.5 }`.
#[tokio::test]
#[ignore = "golden baseline capture; run only via `cargo test -p koharu-renderer --test golden_capture -- --ignored`"]
async fn g2_cleanup_opacity() {
    let (mut session, page) = build_source_with_cleanup(64, 96, true).await;
    capture("g2_cleanup_opacity", &mut session, page).await;
}

/// G3: 128x128 page, checkerboard source (4x4 grid, 32 px cells), plus an
/// Embedded image layer: 16x16 gradient stretched into the 64x64 rectangle
/// at (32,32) -> scaled resampling through the vello image path.
#[tokio::test]
#[ignore = "golden baseline capture; run only via `cargo test -p koharu-renderer --test golden_capture -- --ignored`"]
async fn g3_embedded_scaled() {
    let source_role = AssetRole::new("source").unwrap();
    // 4x4 checkerboard grid over the full 128x128 page (32 px per cell).
    let mut checker = image::RgbaImage::new(128, 128);
    for (x, y, pixel) in checker.enumerate_pixels_mut() {
        let dark = ((x / 32) + (y / 32)) % 2 == 0;
        *pixel = image::Rgba(if dark {
            [20, 20, 20, 255]
        } else {
            [235, 235, 235, 255]
        });
    }
    // 16x16 gradient embedded image.
    let mut gradient = image::RgbaImage::new(16, 16);
    for (x, y, pixel) in gradient.enumerate_pixels_mut() {
        *pixel = image::Rgba([
            (x as u8) * 16,
            (y as u8) * 16,
            128,
            255,
        ]);
    }
    let mut session = Session::memory().await.unwrap();
    let mut page = None;
    let patch = session
        .snapshot()
        .patch(|edit| {
            let created = edit.add_page(PageDraft::new("page", 128.0, 128.0), At::End)?;
            edit.set_asset(
                created,
                &source_role,
                asset(encode_png(checker), 128, 128),
            )?;
            let embedded = edit.add_entity(created, At::End)?;
            edit.set(
                embedded,
                &Geometry::rectangle(32.0, 32.0, 64.0, 64.0),
            )?;
            edit.set_asset(embedded, &source_role, asset(encode_png(gradient), 16, 16))?;
            page = Some(created);
            Ok(())
        })
        .unwrap();
    let _ = session.commit(patch).await.unwrap();
    capture("g3_embedded_scaled", &mut session, page.unwrap()).await;
}

/// G4: 64x96 page with source + Cleanup (same as G1) plus one horizontal
/// Chinese text layer over the images.
#[tokio::test]
#[ignore = "golden baseline capture; run only via `cargo test -p koharu-renderer --test golden_capture -- --ignored`"]
async fn g4_text_over_images() {
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
    capture("g4_text_over_images", &mut session, page).await;
}
