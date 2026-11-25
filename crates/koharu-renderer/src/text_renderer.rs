//! Text layout and Vello glyph recording.

use std::sync::Arc;

use anyhow::Result;
use vello::{
    FontEmbolden, Glyph, Scene,
    kurbo::{Affine, Diagonal2, Join, Stroke},
    peniko::Fill,
};

use crate::{
    Error, FontStyle, HyphenationPolicy, LayoutRun, RenderBounds, RenderDiagnostic,
    Result as RenderResult, TextAlign, TextLayout, WritingMode,
    bubble::LayoutBox,
    fonts::{Fonts, font_key},
    raster::rgba,
    script::is_chinese_or_japanese_text,
};

/// How wide a stroke may get relative to the type size it outlines. A stroke
/// wider than a fifth of the glyph is not an outline: it is the art around the
/// lettering measured as one, and painting a thick ring in that colour is worse
/// than painting none.
const MAX_STROKE_RATIO: f32 = 0.2;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TextNodeDescriptor {
    pub(crate) entity: koharu_scene::EntityId,
    pub(crate) text: String,
    pub(crate) language: Option<koharu_scene::LanguageTag>,
    pub(crate) width: f32,
    pub(crate) height: f32,
    pub(crate) balloon_contour: Option<Vec<(f32, f32)>>,
    pub(crate) preferred_font: Option<String>,
    pub(crate) font_families: Vec<String>,
    pub(crate) font_weight: Option<u16>,
    pub(crate) font_style: Option<FontStyle>,
    pub(crate) font_size: Option<f32>,
    pub(crate) minimum_font_size: f32,
    pub(crate) auto_fit: bool,
    pub(crate) alignment: TextAlign,
    pub(crate) writing_mode: WritingMode,
    pub(crate) foreground_color: [u8; 4],
    pub(crate) stroke: Option<StrokeOptions>,
    pub(crate) line_height: f32,
    pub(crate) letter_spacing: f32,
    pub(crate) word_spacing: f32,
    pub(crate) text_inset: [f32; 4],
    pub(crate) point_text: bool,
    /// The box is the detected `FitsTo` region, so its geometry is the only
    /// statement about how much room the text may use.
    pub(crate) is_fit_region: bool,
    pub(crate) page_width: f32,
    pub(crate) page_height: f32,
}

pub(crate) struct RenderedTextNode {
    pub(crate) scene: Arc<Scene>,
    pub(crate) local_bounds: RenderBounds,
    pub(crate) metadata: RenderedTextMetadata,
    pub(crate) diagnostics: Vec<RenderDiagnostic>,
}

pub(crate) struct RenderedTextMetadata {
    pub(crate) rendered_bounds: RenderBounds,
    pub(crate) layout_bounds: RenderBounds,
    pub(crate) post_script_fonts: Vec<String>,
    pub(crate) font_size: f32,
    pub(crate) color: [u8; 4],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StrokeOptions {
    pub color: [u8; 4],
    pub width_px: f32,
}

/// Paint options used when recording one laid-out text run into a Vello scene.
#[derive(Debug, Clone)]
pub(crate) struct TextRenderOptions {
    pub color: [u8; 4],
    pub hint_glyphs: bool,
    pub padding: f32,
    pub baseline_shift: f32,
    pub stroke: Option<StrokeOptions>,
}

impl Default for TextRenderOptions {
    fn default() -> Self {
        Self {
            color: [0, 0, 0, 255],
            hint_glyphs: true,
            padding: 0.0,
            baseline_shift: 0.0,
            stroke: None,
        }
    }
}

/// Shapes text and records the resulting glyphs into vector scenes.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TextRenderer;

impl TextRenderer {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub(crate) fn layout<'a>(&self, builder: &TextLayout<'a>, text: &str) -> Result<LayoutRun<'a>> {
        builder.run(text)
    }

    pub(crate) fn render(
        &self,
        scene: &mut Scene,
        layout: &LayoutRun<'_>,
        writing_mode: WritingMode,
        options: &TextRenderOptions,
        transform: Affine,
    ) {
        if let Some(stroke) = options
            .stroke
            .filter(|stroke| stroke.width_px > 0.0 && stroke.color[3] > 0)
        {
            draw_layout(
                scene,
                layout,
                writing_mode,
                options,
                transform,
                DrawStyle::Stroke(stroke),
            );
        }
        draw_layout(
            scene,
            layout,
            writing_mode,
            options,
            transform,
            DrawStyle::Fill,
        );
    }

    pub(crate) fn render_descriptor(
        &self,
        descriptor: &TextNodeDescriptor,
        fonts: &Fonts,
    ) -> RenderResult<RenderedTextNode> {
        let is_bubble_text = descriptor.balloon_contour.is_some();
        let frame = LayoutBox {
            x: 0.0,
            y: 0.0,
            width: descriptor.width,
            height: descriptor.height,
        };
        let bounds = if is_bubble_text {
            inset(frame, descriptor.text_inset)
        } else if descriptor.is_fit_region {
            let insetted = inset(frame, fit_text_inset(descriptor, frame));
            if insetted.width > 0.0 && insetted.height > 0.0 {
                insetted
            } else {
                frame
            }
        } else {
            frame
        };
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return Err(Error::invalid(format!(
                "text inset leaves no layout area for entity {}",
                descriptor.entity
            )));
        }
        let fonts = fonts
            .resolve(
                descriptor.preferred_font.as_deref(),
                descriptor.font_weight,
                descriptor.font_style,
                &descriptor.font_families,
                &descriptor.text,
                descriptor
                    .language
                    .as_ref()
                    .map(koharu_scene::LanguageTag::as_str),
            )
            .map_err(|source| Error::Font {
                entity: descriptor.entity,
                source,
            })?;
        let automatic_maximum = || automatic_maximum(descriptor, bounds);
        let maximum = if descriptor.auto_fit {
            automatic_maximum()
        } else {
            descriptor.font_size.unwrap_or_else(automatic_maximum)
        };
        let minimum = descriptor.minimum_font_size.min(maximum);
        let mut layout = TextLayout::new(&fonts[0])
            .with_fallback_fonts(&fonts[1..])
            .with_writing_mode(descriptor.writing_mode)
            .with_alignment(descriptor.alignment)
            .with_line_height(descriptor.line_height)
            .with_spacing(descriptor.letter_spacing, descriptor.word_spacing)
            .with_cjk_punctuation_layout(
                is_chinese_or_japanese_text(&descriptor.text)
                    || descriptor
                        .language
                        .as_ref()
                        .is_some_and(|language| is_chinese_or_japanese_language(language.as_str())),
            );
        if !descriptor.point_text {
            layout = layout
                .with_max_width(bounds.width)
                .with_max_height(bounds.height);
        }
        if let Some(contour) = &descriptor.balloon_contour {
            let [top, _, _, left] = descriptor.text_inset;
            let contour = contour.iter().map(|&(x, y)| (x - left, y - top)).collect();
            layout = layout.with_comic_balloon(
                bounds.width,
                bounds.height,
                contour,
                descriptor.text_inset.into_iter().fold(0.0, f32::max),
            );
        }
        if let Some(language) = &descriptor.language {
            layout = layout.with_hyphenation_language_tag(language.as_str());
        }
        if is_bubble_text && descriptor.writing_mode == WritingMode::Horizontal {
            layout = layout.with_hyphenation_policy(HyphenationPolicy::LastResort);
        }
        let layout = if descriptor.auto_fit && !descriptor.point_text {
            layout
                .with_max_font_size(maximum)
                .with_min_font_size(minimum)
        } else {
            layout.with_font_size(descriptor.font_size.unwrap_or(maximum))
        };
        let layout = self
            .layout(&layout, &descriptor.text)
            .map_err(|source| Error::Layout {
                entity: descriptor.entity,
                source,
            })?;
        let (mut x, mut y) = if descriptor.point_text {
            (bounds.x, bounds.y)
        } else {
            placement(bounds, layout.width, layout.height)
        };
        x += layout.placement_offset_x();
        y += layout.placement_offset_y();
        let transform = Affine::translate((f64::from(x), f64::from(y)));
        let color = descriptor.foreground_color;
        // The detection stage measures a stroke in page pixels, which means
        // nothing until the layout has chosen a type size: it cannot see the
        // fitted size, so a plausible measurement can arrive far too wide for
        // the glyphs it outlines and paint neighbouring strokes into one solid
        // block. A measurement past `MAX_STROKE_RATIO` is not a stroke at all, so
        // the layer goes without one; that decision drives both the paint and the
        // bounds, so the two cannot disagree. The insets keep the measured width:
        // a generous inset only shrinks the layout area, which is the safe
        // direction.
        let stroke = descriptor
            .stroke
            .filter(|stroke| stroke.width_px.max(0.0) <= layout.font_size * MAX_STROKE_RATIO);
        let mut options = TextRenderOptions {
            color,
            stroke: None,
            ..TextRenderOptions::default()
        };
        let mut scene = Scene::new();
        if let Some(stroke) = stroke {
            options.stroke = Some(stroke);
        }
        self.render(
            &mut scene,
            &layout,
            descriptor.writing_mode,
            &options,
            transform,
        );
        let mut diagnostics = Vec::new();
        if layout.font_size + f32::EPSILON < descriptor.minimum_font_size {
            diagnostics.push(RenderDiagnostic::TextBelowReadableSize {
                entity: descriptor.entity,
                font_size: layout.font_size,
                minimum_font_size: descriptor.minimum_font_size,
            });
        }
        if layout.overflowed() {
            diagnostics.push(RenderDiagnostic::TextOverflow {
                entity: descriptor.entity,
                available: bounds.into(),
                actual_width: layout.width,
                actual_height: layout.height,
                font_size: layout.font_size,
            });
        }
        let rendered_bounds = RenderBounds {
            x,
            y,
            width: layout.width,
            height: layout.height,
        };
        let stroke_padding = stroke.map_or(0.0, |stroke| stroke.width_px.max(0.0));
        Ok(RenderedTextNode {
            scene: Arc::new(scene),
            local_bounds: RenderBounds {
                x: rendered_bounds.x - stroke_padding,
                y: rendered_bounds.y - stroke_padding,
                width: rendered_bounds.width + stroke_padding * 2.0,
                height: rendered_bounds.height + stroke_padding * 2.0,
            },
            metadata: RenderedTextMetadata {
                rendered_bounds,
                layout_bounds: if descriptor.point_text {
                    rendered_bounds
                } else {
                    bounds.into()
                },
                post_script_fonts: fonts
                    .iter()
                    .map(|font| font.post_script_name().to_owned())
                    .collect(),
                font_size: layout.font_size,
                color,
            },
            diagnostics,
        })
    }
}

fn automatic_maximum(descriptor: &TextNodeDescriptor, bounds: LayoutBox) -> f32 {
    if descriptor.balloon_contour.is_some() {
        return if descriptor.writing_mode.is_vertical() {
            bounds.height
        } else {
            bounds.width
        };
    }
    if !descriptor.is_fit_region || descriptor.point_text || !descriptor.auto_fit {
        return 24.0;
    }
    let geometric = bounds.width.min(bounds.height);
    if visible_char_count(&descriptor.text) > 3 {
        return geometric;
    }
    // A few characters in a loose detected box would otherwise be blown up to
    // poster lettering; cap them with a reference size derived from the page
    // resolution so the same page always yields the same short caption size.
    let page_reference = descriptor.page_width.min(descriptor.page_height) * 0.028;
    geometric.min((page_reference * 1.8).max(24.0))
}

/// Free text has no balloon padding, so the stroke bleed is folded into the
/// inset; without it the outline crosses the detected box and touches the
/// neighbouring panel border. The inset is capped at a quarter of the short
/// edge so a small detected box keeps a layout area.
fn fit_text_inset(descriptor: &TextNodeDescriptor, frame: LayoutBox) -> [f32; 4] {
    let bleed = descriptor
        .stroke
        .map_or(0.0, |stroke| stroke.width_px.max(0.0));
    let cap = frame.width.min(frame.height) * 0.25;
    descriptor
        .text_inset
        .map(|edge| (edge + bleed).clamp(0.0, cap))
}

fn visible_char_count(text: &str) -> usize {
    text.chars()
        .filter(|character| !character.is_whitespace())
        .count()
}

fn is_chinese_or_japanese_language(language: &str) -> bool {
    language
        .split(['-', '_'])
        .next()
        .is_some_and(|primary| matches!(primary.to_ascii_lowercase().as_str(), "ja" | "zh"))
}

fn inset(rect: LayoutBox, [top, right, bottom, left]: [f32; 4]) -> LayoutBox {
    LayoutBox {
        x: rect.x + left,
        y: rect.y + top,
        width: (rect.width - left - right).max(0.0),
        height: (rect.height - top - bottom).max(0.0),
    }
}

fn placement(rect: LayoutBox, width: f32, height: f32) -> (f32, f32) {
    let x = rect.x + (rect.width - width) * 0.5;
    let remaining = rect.height - height;
    let y = rect.y + remaining * 0.5;
    (x, y)
}

#[derive(Clone, Copy)]
enum DrawStyle {
    Stroke(StrokeOptions),
    Fill,
}

fn draw_layout(
    scene: &mut Scene,
    layout: &LayoutRun<'_>,
    writing_mode: WritingMode,
    options: &TextRenderOptions,
    transform: Affine,
    style: DrawStyle,
) {
    for line in &layout.lines {
        let (baseline_x, baseline_y) = match writing_mode {
            WritingMode::Horizontal | WritingMode::VerticalRl => line.baseline,
        };
        let mut pen_x = 0.0;
        let mut pen_y = 0.0;
        let mut start = 0;

        while start < line.glyphs.len() {
            let font = line.glyphs[start].font;
            let key = font_key(font);
            // A turned glyph is drawn by its own run: `glyph_transform` applies
            // in glyph space, before the position offsets, so one run per
            // orientation keeps the rotation about each pen position.
            let rotate = line.glyphs[start].rotate;
            let mut end = start + 1;
            while end < line.glyphs.len()
                && font_key(line.glyphs[end].font) == key
                && line.glyphs[end].rotate == rotate
            {
                end += 1;
            }

            let mut glyphs = Vec::with_capacity(end - start);
            for glyph in &line.glyphs[start..end] {
                glyphs.push(Glyph {
                    id: glyph.glyph_id,
                    x: options.padding + baseline_x + pen_x + glyph.x_offset,
                    y: options.padding + baseline_y + pen_y
                        - glyph.y_offset
                        - options.baseline_shift,
                });
                pen_x += glyph.x_advance;
                pen_y -= glyph.y_advance;
            }

            let font_data = font.vello_data();
            let normalized_coords = font.normalized_coords();
            let mut run = scene
                .draw_glyphs(&font_data)
                .font_size(layout.font_size)
                .transform(transform)
                .hint(options.hint_glyphs);
            if !normalized_coords.is_empty() {
                run = run.normalized_coords(normalized_coords);
            }
            if let Some(angle) = font.synthetic_skew() {
                let skew = Affine::skew(-(angle.to_radians().tan() as f64), 0.0);
                run = run.glyph_transform(Some(if rotate {
                    skew * Affine::rotate(std::f64::consts::FRAC_PI_2)
                } else {
                    skew
                }));
            } else if rotate {
                run = run.glyph_transform(Some(Affine::rotate(std::f64::consts::FRAC_PI_2)));
            }
            if font.synthetic_bold() {
                run = run.font_embolden(FontEmbolden::new(Diagonal2::new(1.0, 1.0)));
            }

            match style {
                DrawStyle::Fill => run
                    .brush(rgba(options.color))
                    .draw(Fill::NonZero, glyphs.into_iter()),
                DrawStyle::Stroke(stroke) => {
                    let outline =
                        Stroke::new((stroke.width_px * 2.0) as f64).with_join(Join::Round);
                    run.brush(rgba(stroke.color))
                        .draw(&outline, glyphs.into_iter());
                }
            }
            start = end;
        }
    }
}

#[cfg(test)]
mod tests {
    use koharu_scene::EntityId;

    use super::*;

    fn descriptor() -> TextNodeDescriptor {
        TextNodeDescriptor {
            entity: EntityId::new(),
            text: "Hi".to_owned(),
            language: None,
            width: 240.0,
            height: 120.0,
            balloon_contour: None,
            preferred_font: Some("Arial".to_owned()),
            font_families: vec!["Arial".to_owned()],
            font_weight: None,
            font_style: None,
            font_size: Some(6.0),
            minimum_font_size: 9.0,
            auto_fit: true,
            alignment: TextAlign::Center,
            writing_mode: WritingMode::Horizontal,
            foreground_color: [0, 0, 0, 255],
            stroke: None,
            line_height: 1.2,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            text_inset: [0.0; 4],
            point_text: false,
            is_fit_region: false,
            page_width: 1528.0,
            page_height: 2047.0,
        }
    }

    fn bounds() -> LayoutBox {
        LayoutBox {
            x: 0.0,
            y: 0.0,
            width: 240.0,
            height: 120.0,
        }
    }

    #[test]
    fn automatic_size_uses_the_short_edge_for_fit_regions() {
        let descriptor = TextNodeDescriptor {
            text: "一二三四".to_owned(),
            is_fit_region: true,
            ..descriptor()
        };

        assert_eq!(automatic_maximum(&descriptor, bounds()), 120.0);
    }

    #[test]
    fn automatic_size_keeps_the_default_for_non_fit_text() {
        let descriptor = TextNodeDescriptor {
            text: "一二三四".to_owned(),
            ..descriptor()
        };

        assert_eq!(automatic_maximum(&descriptor, bounds()), 24.0);
    }

    #[test]
    fn automatic_size_keeps_the_default_for_point_text() {
        let descriptor = TextNodeDescriptor {
            text: "一二三四".to_owned(),
            is_fit_region: true,
            point_text: true,
            ..descriptor()
        };

        assert_eq!(automatic_maximum(&descriptor, bounds()), 24.0);
    }

    #[test]
    fn automatic_size_caps_short_fit_regions_by_page_resolution() {
        let descriptor = TextNodeDescriptor {
            text: "你好".to_owned(),
            is_fit_region: true,
            ..descriptor()
        };

        assert_eq!(
            automatic_maximum(&descriptor, bounds()),
            1528.0_f32.min(2047.0) * 0.028 * 1.8
        );
    }

    #[test]
    fn fit_text_inset_adds_stroke_bleed_and_stops_at_the_short_edge() {
        let descriptor = TextNodeDescriptor {
            text_inset: [4.0; 4],
            stroke: Some(StrokeOptions {
                color: [0, 0, 0, 255],
                width_px: 6.0,
            }),
            ..descriptor()
        };
        assert_eq!(
            fit_text_inset(
                &descriptor,
                LayoutBox {
                    x: 0.0,
                    y: 0.0,
                    width: 250.0,
                    height: 450.0,
                }
            ),
            [10.0; 4]
        );
        assert_eq!(
            fit_text_inset(
                &descriptor,
                LayoutBox {
                    x: 0.0,
                    y: 0.0,
                    width: 4.0,
                    height: 4.0,
                }
            ),
            [1.0; 4]
        );
    }

    /// A page-sized vertical fit region holding twenty characters, the shape a
    /// detected block of Japanese becomes once it is translated.
    fn fitted_block(stroke_width: f32, family: String) -> TextNodeDescriptor {
        TextNodeDescriptor {
            text: "这一块文字用来检查描边宽度会不会把整块糊成实心".to_owned(),
            width: 285.0,
            height: 344.0,
            preferred_font: Some(family.clone()),
            font_families: vec![family],
            font_size: None,
            writing_mode: WritingMode::VerticalRl,
            auto_fit: true,
            is_fit_region: true,
            text_inset: [4.0; 4],
            stroke: Some(StrokeOptions {
                color: [187, 97, 71, 255],
                width_px: stroke_width,
            }),
            ..descriptor()
        }
    }

    /// The one font these cases shape with, loaded before shaping.
    async fn prepared_family() -> (String, Fonts) {
        let family = crate::fonts::FontSystem::new()
            .first_font()
            .expect("a system font")
            .family_name()
            .to_owned();
        let fonts = Fonts::new();
        fonts
            .prepare(&[crate::fonts::FontRequest {
                family: family.clone(),
                weight: None,
                style: None,
            }])
            .await
            .expect("the font loads");
        (family, fonts)
    }

    /// The distance the painted stroke adds on each side of the laid-out glyphs.
    fn stroke_bleed(node: &RenderedTextNode) -> f32 {
        node.metadata.rendered_bounds.x - node.local_bounds.x
    }

    #[tokio::test]
    async fn oversized_stroke_is_dropped() {
        let (family, fonts) = prepared_family().await;
        let descriptor = fitted_block(34.0, family);
        let node = TextRenderer::new()
            .render_descriptor(&descriptor, &fonts)
            .expect("the block lays out");

        // 34 pixels around a glyph of about 45 is the art around the lettering,
        // not an outline: the layer goes without one, so nothing is added around
        // the glyphs and the block keeps its box.
        let bleed = stroke_bleed(&node);
        assert_eq!(
            bleed, 0.0,
            "a stroke wider than a fifth of the glyph is dropped, bled {bleed}"
        );
        let bounds = node.local_bounds;
        assert_eq!(bounds, node.metadata.rendered_bounds, "no padding is added");
        assert!(
            bounds.x >= -0.01
                && bounds.y >= -0.01
                && bounds.x + bounds.width <= descriptor.width + 0.01
                && bounds.y + bounds.height <= descriptor.height + 0.01,
            "the block stays inside its frame: {bounds:?}"
        );
    }

    #[tokio::test]
    async fn vertical_ellipsis_ink_stacks_vertically() {
        let (family, fonts) = prepared_family().await;
        let render = |writing_mode| {
            let descriptor = TextNodeDescriptor {
                text: "\u{2026}\u{2026}".to_owned(),
                width: 200.0,
                height: 400.0,
                preferred_font: Some(family.clone()),
                font_families: vec![family.clone()],
                font_size: None,
                writing_mode,
                auto_fit: true,
                is_fit_region: true,
                text_inset: [4.0; 4],
                ..descriptor()
            };
            TextRenderer::new()
                .render_descriptor(&descriptor, &fonts)
                .expect("the ellipses lay out")
        };

        // The layout box alone cannot carry this: a vertical run is taller than
        // it is wide whether or not the ink is turned, so the glyph flags are
        // checked too, with the same family shaped directly.
        let font = crate::fonts::FontSystem::new()
            .query_family(&family)
            .expect("the prepared family resolves");
        let shaped = TextLayout::new(&font)
            .with_font_size(64.0)
            .with_writing_mode(WritingMode::VerticalRl)
            .run("\u{2026}\u{2026}")
            .expect("the ellipses shape");
        assert!(
            shaped.lines[0].glyphs.iter().all(|glyph| glyph.rotate),
            "the vertical ellipses are turned: {:?}",
            shaped.lines[0].glyphs
        );

        let vertical = render(WritingMode::VerticalRl);
        let horizontal = render(WritingMode::Horizontal);
        assert!(
            vertical.metadata.rendered_bounds.height > vertical.metadata.rendered_bounds.width,
            "turned ellipses stack down the column: {:?}",
            vertical.metadata.rendered_bounds
        );
        assert!(
            horizontal.metadata.rendered_bounds.width > horizontal.metadata.rendered_bounds.height,
            "upright ellipses lie along the line: {:?}",
            horizontal.metadata.rendered_bounds
        );
    }

    #[tokio::test]
    async fn normal_stroke_is_left_alone() {
        let (family, fonts) = prepared_family().await;
        let descriptor = fitted_block(6.0, family);
        let node = TextRenderer::new()
            .render_descriptor(&descriptor, &fonts)
            .expect("the block lays out");

        let bleed = stroke_bleed(&node);
        assert!(
            (bleed - 6.0).abs() <= 0.01,
            "a normal stroke keeps its paint width, bled {bleed}"
        );
    }
}
