use std::{collections::BTreeMap, io::Cursor, sync::Arc};

use koharu_renderer::{ImageKind, LayerKind, RasterOptions, Renderer};
use koharu_scene::{
    AssetInput, AssetMetadata, AssetRole, At, Origin, PageDraft, RasterLayer, RasterLayerKind,
    Session,
};

fn png(width: u32, height: u32, color: [u8; 4]) -> Arc<[u8]> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba(color));
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner().into()
}

fn png_image(image: image::RgbaImage) -> Arc<[u8]> {
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner().into()
}

#[tokio::test]
async fn public_renderer_returns_a_complete_immutable_frame() {
    let mut session = Session::memory().await.unwrap();
    let mut page = None;
    let patch = session
        .snapshot()
        .patch(|edit| {
            page = Some(edit.add_page(PageDraft::new("page", 320.0, 240.0), At::End)?);
            Ok(())
        })
        .unwrap();
    let snapshot = session.commit(patch).await.unwrap().snapshot;
    let page = page.unwrap();

    let renderer = Renderer::new().unwrap();
    let frame = renderer.render(&snapshot, page).await.unwrap();

    assert_eq!(frame.page(), page);
    assert_eq!(frame.revision(), snapshot.revision());
    assert_eq!(frame.size(), (320, 240));
    assert_eq!(frame.origin(), (0, 0));
    assert!(frame.layers().is_empty());
    assert!(frame.layer(page).is_none());
    assert!(frame
        .diagnostics()
        .iter()
        .any(|diagnostic| matches!(diagnostic, koharu_renderer::RenderDiagnostic::MissingAsset { entity, role } if *entity == page && role == "source")));
}

#[tokio::test]
async fn discarded_image_nodes_are_rebuilt_for_a_reopened_presentation() {
    let mut session = Session::memory().await.unwrap();
    let source = AssetRole::new("source").unwrap();
    let color = [12, 34, 56, 255];
    let mut page = None;
    let patch = session
        .snapshot()
        .patch(|edit| {
            let created = edit.add_page(PageDraft::new("page", 4.0, 4.0), At::End)?;
            edit.set_asset(
                created,
                &source,
                AssetInput::new(
                    png(4, 4, color),
                    "image/png",
                    AssetMetadata {
                        width: Some(4),
                        height: Some(4),
                        attributes: BTreeMap::new(),
                    },
                ),
            )?;
            page = Some(created);
            Ok(())
        })
        .unwrap();
    let snapshot = session.commit(patch).await.unwrap().snapshot;
    let page = page.unwrap();
    let renderer = Renderer::new().unwrap();

    renderer.render(&snapshot, page).await.unwrap();
    renderer.discard_retained_nodes();
    let reopened = renderer.render(&snapshot, page).await.unwrap();
    let raster = renderer
        .rasterize(&reopened, RasterOptions::default())
        .await
        .unwrap();

    assert_eq!(reopened.stats().rebuilt_layers, 1);
    assert!(raster.image.pixels().all(|pixel| pixel.0 == color));
}

#[tokio::test]
async fn rasterized_png_contains_source_and_cleanup_pixels() {
    let mut session = Session::memory().await.unwrap();
    let source = AssetRole::new("source").unwrap();
    let mut cleanup = image::RgbaImage::new(4, 4);
    cleanup.put_pixel(2, 1, image::Rgba([240, 30, 20, 255]));
    let mut page = None;
    let patch = session
        .snapshot()
        .patch(|edit| {
            let created = edit.add_page(PageDraft::new("page", 4.0, 4.0), At::End)?;
            edit.set_asset(
                created,
                &source,
                AssetInput::new(
                    png(4, 4, [10, 20, 200, 255]),
                    "image/png",
                    AssetMetadata {
                        width: Some(4),
                        height: Some(4),
                        attributes: BTreeMap::new(),
                    },
                ),
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
                &source,
                AssetInput::new(
                    png_image(cleanup),
                    "image/png",
                    AssetMetadata {
                        width: Some(4),
                        height: Some(4),
                        attributes: BTreeMap::new(),
                    },
                ),
            )?;
            page = Some(created);
            Ok(())
        })
        .unwrap();
    let snapshot = session.commit(patch).await.unwrap().snapshot;
    let renderer = Renderer::new().unwrap();
    let frame = renderer.render(&snapshot, page.unwrap()).await.unwrap();
    let raster = renderer
        .rasterize(&frame, RasterOptions::default())
        .await
        .unwrap();

    assert!(frame.layers().iter().any(|layer| matches!(
        layer.kind(),
        LayerKind::Image(metadata) if metadata.kind == ImageKind::Source
    )));
    assert!(frame.layers().iter().any(|layer| matches!(
        layer.kind(),
        LayerKind::Image(metadata) if metadata.kind == ImageKind::Cleanup
    )));
    assert_eq!(raster.image.get_pixel(0, 0).0, [10, 20, 200, 255]);
    assert_eq!(raster.image.get_pixel(2, 1).0, [240, 30, 20, 255]);
}

#[test]
fn public_layer_kinds_remain_export_metadata() {
    let _ = LayerKind::Image(koharu_renderer::ImageMetadata {
        name: None,
        kind: ImageKind::Source,
    });
}

/// Straight-alpha src-over: `src` composited over `dst`, channels rounded to
/// nearest. Used to derive expected composite colors independently of the
/// implementation under test.
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

/// Per-page color pair (opaque source, translucent Cleanup), varied by page
/// index so cross-page bleed would be visible.
fn regression_page_colors(index: usize) -> ([u8; 4], [u8; 4]) {
    let i = u8::try_from(index).expect("page index fits in u8");
    (
        [10 + 40 * i, 20 + 30 * i, 100 + 10 * i, 255],
        [240 - 30 * i, 30 + 20 * i, 20 + 40 * i, 128],
    )
}

/// 回归(验收标准第 3 条):同一个 `Renderer` 实例顺序 render+rasterize 6 页
/// (≥5),每页两张 ≥2000×3000 整页图层(source 纯色不透明 + Cleanup 半透明)。
/// 这正是原 bug 场景:旧 vello 图集实现在第 3、6 次 rasterize 时会静默丢掉
/// 图像层,页面退回透明。这里断言每页多个抽样像素不透明且等于独立按
/// src-over 算出的预期合成色。纯软件路径,无需 GPU。
#[tokio::test]
async fn sequential_render_rasterize_keeps_image_layers_on_every_page() {
    const WIDTH: u32 = 2000;
    const HEIGHT: u32 = 3000;
    const PAGES: usize = 6;
    const SAMPLES: [(u32, u32); 6] = [
        (0, 0),
        (WIDTH - 1, 0),
        (0, HEIGHT - 1),
        (WIDTH - 1, HEIGHT - 1),
        (1000, 1500),
        (777, 2222),
    ];

    let source = AssetRole::new("source").unwrap();
    let asset_meta = |color: [u8; 4]| {
        AssetInput::new(
            png(WIDTH, HEIGHT, color),
            "image/png",
            AssetMetadata {
                width: Some(WIDTH),
                height: Some(HEIGHT),
                attributes: BTreeMap::new(),
            },
        )
    };
    let mut session = Session::memory().await.unwrap();
    let mut pages: Vec<koharu_scene::EntityId> = Vec::new();
    let patch = session
        .snapshot()
        .patch(|edit| {
            for index in 0..PAGES {
                let (src, clean) = regression_page_colors(index);
                let created = edit.add_page(
                    PageDraft::new(format!("page{index}"), f64::from(WIDTH), f64::from(HEIGHT)),
                    At::End,
                )?;
                edit.set_asset(created, &source, asset_meta(src))?;
                let cleanup_layer = edit.add_entity(created, At::Start)?;
                edit.set(
                    cleanup_layer,
                    &RasterLayer {
                        origin: Origin::User,
                        name: "Cleanup".to_owned(),
                        kind: RasterLayerKind::Cleanup,
                    },
                )?;
                edit.set_asset(cleanup_layer, &source, asset_meta(clean))?;
                pages.push(created);
            }
            Ok(())
        })
        .unwrap();
    let snapshot = session.commit(patch).await.unwrap().snapshot;

    // One renderer for every page, in sequence — retention across renders is
    // exactly what the old atlas bug corrupted.
    let renderer = Renderer::new().unwrap();
    for (index, &page) in pages.iter().enumerate() {
        let (src, clean) = regression_page_colors(index);
        let expected = src_over(src, clean);

        let frame = renderer.render(&snapshot, page).await.unwrap();
        assert_eq!(frame.size(), (WIDTH, HEIGHT), "page {index}: frame size");
        assert_eq!(
            frame.layers().len(),
            2,
            "page {index}: expected exactly the source + Cleanup image layers"
        );
        assert!(
            matches!(frame.layers()[0].kind(), LayerKind::Image(meta) if meta.kind == ImageKind::Source),
            "page {index}: first layer must be the Source image"
        );
        assert!(
            matches!(frame.layers()[1].kind(), LayerKind::Image(meta) if meta.kind == ImageKind::Cleanup),
            "page {index}: second layer must be the Cleanup image"
        );

        let raster = renderer
            .rasterize(&frame, RasterOptions::default())
            .await
            .unwrap();
        assert_eq!(
            raster.image.dimensions(),
            (WIDTH, HEIGHT),
            "page {index}: raster dimensions"
        );

        for &(x, y) in &SAMPLES {
            let pixel = raster.image.get_pixel(x, y).0;
            assert_eq!(
                pixel[3], 255,
                "page {index} pixel ({x},{y}): image layer was dropped \
                 (transparent pixel {pixel:?}), expected opaque {expected:?}"
            );
            for channel in 0..3 {
                let diff = i32::from(pixel[channel]) - i32::from(expected[channel]);
                assert!(
                    diff.abs() <= 1,
                    "page {index} pixel ({x},{y}): got {pixel:?}, expected src-over \
                     {expected:?} (src {src:?} over-composited by cleanup {clean:?})"
                );
            }
        }
        println!("regression page {index}: sampled {SAMPLES:?} all opaque, expected {expected:?}");
    }
}
