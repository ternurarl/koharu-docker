//! Reusable Vello rasterization into CPU-readable images.

use std::sync::mpsc;

use anyhow::{Context, Result, anyhow, bail};
use fast_image_resize::{FilterType, ResizeAlg, ResizeOptions, Resizer};
use image::RgbaImage;
use parking_lot::Mutex;
use vello::wgpu::{
    self, Buffer, BufferDescriptor, BufferUsages, CommandEncoderDescriptor, Extent3d,
    TexelCopyBufferInfo, Texture, TextureDescriptor, TextureFormat, TextureUsages, TextureView,
};
use vello::{
    AaConfig, AaSupport, RenderParams, RendererOptions, Scene, kurbo::Affine, peniko::Color,
};

use crate::compose::Canvas;
use crate::frame::{Layer, LayerKind};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DownsampleFilter {
    Nearest,
    Triangle,
    CatmullRom,
    Gaussian,
    #[default]
    Lanczos3,
}

impl From<DownsampleFilter> for ResizeAlg {
    fn from(value: DownsampleFilter) -> Self {
        match value {
            DownsampleFilter::Nearest => ResizeAlg::Nearest,
            DownsampleFilter::Triangle => ResizeAlg::Convolution(FilterType::Bilinear),
            DownsampleFilter::CatmullRom => ResizeAlg::Convolution(FilterType::CatmullRom),
            DownsampleFilter::Gaussian => ResizeAlg::Convolution(FilterType::Gaussian),
            DownsampleFilter::Lanczos3 => ResizeAlg::Convolution(FilterType::Lanczos3),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterOptions {
    pub supersampling_factor: u32,
    pub downsample_filter: DownsampleFilter,
}

impl RasterOptions {
    #[must_use]
    pub fn supersampled(factor: u32) -> Self {
        Self {
            supersampling_factor: factor,
            ..Default::default()
        }
    }

    fn scale(self) -> u32 {
        self.supersampling_factor.clamp(1, MAX_SUPERSAMPLING_FACTOR)
    }
}

impl Default for RasterOptions {
    fn default() -> Self {
        Self {
            // Vello already uses area anti-aliasing. Supersampling remains available for
            // exports, but should not multiply every interactive render by four by default.
            supersampling_factor: 1,
            downsample_filter: DownsampleFilter::Lanczos3,
        }
    }
}

const MAX_SUPERSAMPLING_FACTOR: u32 = 4;

/// Mirrors the visibility gate inside `Layer::append_with_presentation`.
fn is_presented(layer: &Layer) -> bool {
    let presentation = layer.presentation();
    presentation.visible && presentation.opacity.is_finite() && presentation.opacity > 0.0
}

#[derive(Debug)]
pub struct Raster {
    pub image: RgbaImage,
    pub left: i32,
    pub top: i32,
}

struct GpuState {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: vello::Renderer,
    targets: TargetPool,
}

/// The render surface and readback buffer for one output size.
struct RenderTarget {
    width: u32,
    height: u32,
    padded_width: u32,
    texture: Texture,
    view: TextureView,
    readback: Buffer,
}

/// How many render targets the pool keeps.
const MAX_POOLED_TARGETS: usize = 4;

/// How many bytes of GPU render targets the pool keeps across calls.
///
/// Upstream commit fbe36bd9 capped its uploaded page textures at 256 MiB and upstream
/// commit 5f0fd18c capped retained pixels. This fork keeps no per-page texture; the GPU
/// memory that survives a render here is a pooled `RenderTarget`, which is an RGBA8
/// texture plus a readback buffer of the same image. The count bound alone left four
/// full pages of both resident.
const MAX_POOLED_TARGET_BYTES: u64 = 256 << 20;

/// Render targets kept for reuse, least recently used first out, bounded by byte budget.
#[derive(Default)]
struct TargetPool {
    /// Each target with the clock value of the call that last took or returned it.
    targets: Vec<(u64, RenderTarget)>,
    clock: u64,
}

impl TargetPool {
    fn next_clock(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    /// Takes a target of exactly this size, or makes one.
    ///
    /// A size the GPU has no memory for fails here and leaves the pool as it was, so no
    /// target the allocation failed for is kept.
    fn take(&mut self, device: &wgpu::Device, width: u32, height: u32) -> Result<RenderTarget> {
        let clock = self.next_clock();
        match self
            .targets
            .iter()
            .position(|(_, target)| target.width == width && target.height == height)
        {
            Some(position) => {
                let mut entry = self.targets.swap_remove(position);
                entry.0 = clock;
                Ok(entry.1)
            }
            None => RenderTarget::new(device, width, height),
        }
    }

    /// Keeps `target` for the next render, evicting the least recently used ones until it
    /// fits the pool's count and byte bounds.
    fn give_back(&mut self, target: RenderTarget) {
        let bytes = target.byte_len();
        // A target heavier than the whole budget is not pooled; keeping it would evict
        // everything else and still exceed what the pool may hold.
        if bytes > MAX_POOLED_TARGET_BYTES {
            return;
        }
        while self.targets.len() >= MAX_POOLED_TARGETS
            || self.retained_bytes().saturating_add(bytes) > MAX_POOLED_TARGET_BYTES
        {
            let Some(oldest) = self
                .targets
                .iter()
                .enumerate()
                .min_by_key(|(_, (clock, _))| *clock)
                .map(|(position, _)| position)
            else {
                break;
            };
            self.targets.remove(oldest);
        }
        let clock = self.next_clock();
        self.targets.push((clock, target));
    }

    fn retained_bytes(&self) -> u64 {
        self.targets
            .iter()
            .map(|(_, target)| target.byte_len())
            .sum()
    }
}

/// A reusable, headless Vello rasterizer.
///
/// GPU setup, Vello's glyph caches, and a small target pool live for the lifetime of this value.
/// Command encoding is serialized because Vello mutates its caches, but GPU completion and
/// readback happen after releasing the lock so independent callers can overlap that work.
pub(crate) struct Rasterizer {
    gpu: Mutex<GpuState>,
}

impl Rasterizer {
    pub(crate) fn new() -> crate::Result<Self> {
        Self::try_new().map_err(crate::Error::Backend)
    }

    fn try_new() -> Result<Self> {
        let (device, queue) = pollster::block_on(request_device())?;
        let renderer = vello::Renderer::new(
            &device,
            RendererOptions {
                antialiasing_support: AaSupport::area_only(),
                ..Default::default()
            },
        )
        .map_err(|error| anyhow!("failed to create Vello renderer: {error:?}"))?;

        Ok(Self {
            gpu: Mutex::new(GpuState {
                device,
                queue,
                renderer,
                targets: TargetPool::default(),
            }),
        })
    }

    /// Composites a frame in strict layer order.
    ///
    /// Image layers are composited on the CPU (`Canvas::overlay_decoded`), so full-page
    /// images never go through Vello's image atlas. Runs of consecutive text layers are
    /// rendered by Vello onto a transparent surface and overlaid as a block, preserving
    /// the original layer interleaving. Pages without any image layer keep the original
    /// single-scene Vello path untouched.
    pub(crate) fn rasterize(
        &self,
        frame: &crate::Frame,
        options: RasterOptions,
    ) -> crate::Result<Raster> {
        let (width, height) = frame.size();
        let (left, top) = frame.origin();
        if !frame
            .layers()
            .iter()
            .any(|layer| matches!(layer.kind(), LayerKind::Image(_)))
        {
            // No image layers: original single-scene Vello path, unchanged.
            let image = self
                .rasterize_scene_inner(frame.scene(), width, height, [0, 0, 0, 0], options)
                .map_err(classify)?;
            return Ok(Raster { image, left, top });
        }
        let image = self.rasterize_layered(frame, options).map_err(classify)?;
        Ok(Raster { image, left, top })
    }

    /// Layered path for pages that contain at least one image layer.
    ///
    /// Visible, axis-aligned image layers with decoded pixels are composited on the CPU;
    /// text runs (and rotated/skewed image-layer fallbacks) are rendered by Vello with a
    /// transparent background and overlaid in place so ordering is never rearranged.
    fn rasterize_layered(&self, frame: &crate::Frame, options: RasterOptions) -> Result<RgbaImage> {
        let (width, height) = frame.size();
        if width == 0 || height == 0 {
            bail!("invalid render surface {width}x{height}");
        }
        let layers = frame.layers();
        let normalization = frame.0.normalization;
        let factor = options.scale();
        let placement = |layer: &Layer| normalization * layer.0.placement;

        // Phase 1: does any image layer actually go through CPU composition?
        let needs_canvas = layers.iter().any(|layer| {
            matches!(layer.kind(), LayerKind::Image(_))
                && is_presented(layer)
                && {
                    let [_, b, c, ..] = placement(layer).as_coeffs();
                    b.abs() <= 1e-9 && c.abs() <= 1e-9
                }
                && layer.decoded_image().is_some()
        });

        if !needs_canvas {
            // Nothing to composite on the CPU: one Vello scene covers every drawable
            // layer (text plus rotated image-layer fallbacks); skipped image layers
            // (hidden, empty, or pixel-less) contribute nothing.
            let mut scene = Scene::new();
            let mut drawable = false;
            for layer in layers {
                match layer.kind() {
                    LayerKind::Text(_) => {
                        if !is_presented(layer) {
                            continue;
                        }
                        layer.append_to(&mut scene, Some(normalization));
                        drawable = true;
                    }
                    LayerKind::Image(_) => {
                        if !is_presented(layer) {
                            continue;
                        }
                        let [_, b, c, ..] = placement(layer).as_coeffs();
                        if b.abs() > 1e-9 || c.abs() > 1e-9 {
                            // Rotation/shear cannot be expressed by overlay_decoded;
                            // fall back to Vello for correctness.
                            layer.append_to(&mut scene, Some(normalization));
                            drawable = true;
                            continue;
                        }
                        if layer.decoded_image().is_none() {
                            continue;
                        }
                        // Unreachable: this layer would have made `needs_canvas` true.
                        layer.append_to(&mut scene, Some(normalization));
                        drawable = true;
                    }
                }
            }
            if !drawable {
                // No text, no fallback: never touch the GPU.
                return Ok(RgbaImage::new(width, height));
            }
            return self.rasterize_scene_inner(&scene, width, height, [0, 0, 0, 0], options);
        }

        let raster_width = width
            .checked_mul(factor)
            .context("supersampled render surface width overflow")?;
        let raster_height = height
            .checked_mul(factor)
            .context("supersampled render surface height overflow")?;
        let mut canvas = Canvas::new(raster_width, raster_height);
        // Consecutive text/fallback layers accumulate here until the next CPU overlay.
        let mut run: Option<Scene> = None;
        for layer in layers {
            match layer.kind() {
                LayerKind::Text(_) => {
                    if !is_presented(layer) {
                        continue;
                    }
                    layer.append_to(run.get_or_insert_with(Scene::new), Some(normalization));
                }
                LayerKind::Image(_) => {
                    let presentation = layer.presentation();
                    if !presentation.visible
                        || !presentation.opacity.is_finite()
                        || presentation.opacity <= 0.0
                    {
                        continue;
                    }
                    let [a, b, c, d, e, f] = placement(layer).as_coeffs();
                    if b.abs() > 1e-9 || c.abs() > 1e-9 {
                        // Rotation/shear fallback: render through Vello inside the
                        // current run so layer order is preserved.
                        layer.append_to(run.get_or_insert_with(Scene::new), Some(normalization));
                        continue;
                    }
                    let Some(decoded) = layer.decoded_image() else {
                        continue;
                    };
                    if let Some(scene) = run.take() {
                        let run_image =
                            self.render_run(&scene, raster_width, raster_height, factor)?;
                        canvas.overlay_rgba_full(&run_image, 1.0);
                    }
                    let scale = f64::from(factor);
                    canvas.overlay_decoded(
                        &decoded,
                        a * scale,
                        d * scale,
                        e * scale,
                        f * scale,
                        presentation.opacity.clamp(0.0, 1.0),
                    );
                }
            }
        }
        if let Some(scene) = run.take() {
            let run_image = self.render_run(&scene, raster_width, raster_height, factor)?;
            canvas.overlay_rgba_full(&run_image, 1.0);
        }
        let mut image = canvas.into_rgba();
        if factor > 1 {
            let mut downsampled = RgbaImage::new(width, height);
            let resize_options = ResizeOptions::new()
                .resize_alg(options.downsample_filter.into())
                .use_alpha(true);
            Resizer::new()
                .resize(&image, &mut downsampled, &resize_options)
                .context("failed to downsample composited render")?;
            image = downsampled;
        }
        Ok(image)
    }

    /// Renders one accumulated run at the (already supersampled) target size with a
    /// transparent background; the whole canvas is downsampled once at the end instead.
    fn render_run(&self, scene: &Scene, width: u32, height: u32, factor: u32) -> Result<RgbaImage> {
        let scaled;
        let scene = if factor == 1 {
            scene
        } else {
            scaled = {
                let mut scaled = Scene::new();
                scaled.append(scene, Some(Affine::scale(f64::from(factor))));
                scaled
            };
            &scaled
        };
        let pixels = self.readback(scene, width, height, [0, 0, 0, 0])?;
        RgbaImage::from_raw(width, height, pixels).context("WGPU returned an invalid RGBA buffer")
    }

    pub(crate) fn rasterize_scene(
        &self,
        scene: &Scene,
        width: u32,
        height: u32,
        background: [u8; 4],
        options: RasterOptions,
    ) -> crate::Result<RgbaImage> {
        self.rasterize_scene_inner(scene, width, height, background, options)
            .map_err(classify)
    }

    fn rasterize_scene_inner(
        &self,
        scene: &Scene,
        width: u32,
        height: u32,
        background: [u8; 4],
        raster: RasterOptions,
    ) -> Result<RgbaImage> {
        if width == 0 || height == 0 {
            bail!("invalid render surface {width}x{height}");
        }
        let scale = raster.scale();
        let raster_width = width
            .checked_mul(scale)
            .context("supersampled render surface width overflow")?;
        let raster_height = height
            .checked_mul(scale)
            .context("supersampled render surface height overflow")?;
        let scaled;
        let scene = if scale == 1 {
            scene
        } else {
            scaled = {
                let mut scaled = Scene::new();
                scaled.append(scene, Some(Affine::scale(scale as f64)));
                scaled
            };
            &scaled
        };
        let pixels = self.readback(scene, raster_width, raster_height, background)?;
        let image = RgbaImage::from_raw(raster_width, raster_height, pixels)
            .context("WGPU returned an invalid RGBA buffer")?;
        if scale == 1 {
            return Ok(image);
        }
        let mut downsampled = RgbaImage::new(width, height);
        let resize_options = ResizeOptions::new()
            .resize_alg(raster.downsample_filter.into())
            .use_alpha(true);
        Resizer::new()
            .resize(&image, &mut downsampled, &resize_options)
            .context("failed to downsample WGPU render")?;
        Ok(downsampled)
    }

    fn readback(
        &self,
        scene: &Scene,
        width: u32,
        height: u32,
        background: [u8; 4],
    ) -> Result<Vec<u8>> {
        let size = Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let (device, submission, target) = {
            let mut gpu = self.gpu.lock();
            let GpuState {
                device,
                queue,
                renderer,
                targets,
            } = &mut *gpu;
            let device = device.clone();
            let limits = device.limits();
            if width > limits.max_texture_dimension_2d || height > limits.max_texture_dimension_2d {
                bail!(
                    "render surface {width}x{height} exceeds the device limit {}",
                    limits.max_texture_dimension_2d
                );
            }
            let target = targets.take(&device, width, height)?;
            let submission = checked(&device, || {
                renderer
                    .render_to_texture(
                        &device,
                        queue,
                        scene,
                        &target.view,
                        &RenderParams {
                            base_color: rgba(background),
                            width,
                            height,
                            antialiasing_method: AaConfig::Area,
                        },
                    )
                    .map_err(|error| anyhow!("Vello rendering failed: {error:?}"))?;

                let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
                    label: Some("koharu text readback encoder"),
                });
                encoder.copy_texture_to_buffer(
                    target.texture.as_image_copy(),
                    TexelCopyBufferInfo {
                        buffer: &target.readback,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(target.padded_width),
                            rows_per_image: None,
                        },
                    },
                    size,
                );
                Ok(queue.submit([encoder.finish()]))
            })?;
            (device, submission, target)
        };
        let slice = target.readback.slice(..);
        let (sender, receiver) = mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })
            .map_err(|error| anyhow!("WGPU device polling failed: {error:?}"))?;
        receiver
            .recv()
            .context("WGPU closed the readback channel")?
            .context("failed to map WGPU readback buffer")?;

        let mapped = slice
            .get_mapped_range()
            .map_err(|error| anyhow!("failed to map WGPU readback buffer: {error:?}"))?;
        let row_len = (width * 4) as usize;
        let mut pixels = Vec::with_capacity(row_len * height as usize);
        for row in mapped
            .chunks_exact(target.padded_width as usize)
            .take(height as usize)
        {
            pixels.extend_from_slice(&row[..row_len]);
        }
        drop(mapped);
        target.readback.unmap();
        // Only a target that rendered and read back is pooled; one the GPU failed on was
        // dropped above and the next render allocates a fresh one.
        let mut gpu = self.gpu.lock();
        gpu.targets.give_back(target);
        Ok(pixels)
    }
}

/// Creates the device as Vello's `RenderContext::device` does, but with the
/// adapter's texture and buffer limits. Vello requests `Limits::default()`,
/// which caps every surface at 8192 px and its readback at 256 MiB whatever
/// the GPU supports.
async fn request_device() -> Result<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        display: None,
        backends: wgpu::Backends::from_env().unwrap_or_default(),
        flags: wgpu::InstanceFlags::from_build_config().with_env(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        backend_options: wgpu::BackendOptions::from_env_or_default(),
    });
    let adapter = wgpu::util::initialize_adapter_from_env_or_default(&instance, None)
        .await
        .context("no WGPU adapter is available")?;
    let supported = adapter.limits();
    let required_limits = wgpu::Limits {
        max_texture_dimension_2d: supported.max_texture_dimension_2d,
        max_buffer_size: supported.max_buffer_size,
        ..wgpu::Limits::default()
    };
    let optional_features = wgpu::Features::CLEAR_TEXTURE | wgpu::Features::PIPELINE_CACHE;
    adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("koharu rasterizer device"),
            required_features: adapter.features() & optional_features,
            required_limits,
            ..Default::default()
        })
        .await
        .context("no WGPU adapter supports Vello's required features")
}

impl RenderTarget {
    fn new(device: &wgpu::Device, width: u32, height: u32) -> Result<Self> {
        let size = Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let row_bytes = width
            .checked_mul(4)
            .context("render target row size overflow")?;
        let padded_width = row_bytes.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer_size = u64::from(padded_width)
            .checked_mul(u64::from(height))
            .context("render target buffer size overflow")?;
        let (texture, readback) = checked(device, || {
            let texture = device.create_texture(&TextureDescriptor {
                label: Some("koharu text target"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: TextureFormat::Rgba8Unorm,
                usage: TextureUsages::STORAGE_BINDING | TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let readback = device.create_buffer(&BufferDescriptor {
                label: Some("koharu text readback"),
                size: buffer_size,
                usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            Ok((texture, readback))
        })?;
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(Self {
            width,
            height,
            padded_width,
            texture,
            view,
            readback,
        })
    }

    /// The GPU memory this target holds: one RGBA8 texture and its readback buffer.
    fn byte_len(&self) -> u64 {
        let texture = u64::from(self.width) * u64::from(self.height) * 4;
        let readback = u64::from(self.padded_width) * u64::from(self.height);
        texture.saturating_add(readback)
    }
}

/// Marks a failure as the GPU having no memory, so `classify` can report it apart from the
/// validation errors wgpu raises for whatever used the resource it could not make.
#[derive(Debug)]
struct OutOfMemory(String);

impl std::fmt::Display for OutOfMemory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for OutOfMemory {}

/// Turns a wgpu failure into the renderer's own error, keeping running out of memory apart
/// from the validation errors it causes.
fn classify(error: anyhow::Error) -> crate::Error {
    match error.downcast::<OutOfMemory>() {
        Ok(out_of_memory) => crate::Error::OutOfMemory(out_of_memory.0),
        Err(error) => crate::Error::Backend(error),
    }
}

/// Runs `work`, which makes a GPU resource or records GPU work, inside error scopes.
///
/// Without them wgpu panics on an uncaptured error and a render that failed leaves what it
/// read back blank. Running out of memory is reported ahead of the validation errors: wgpu
/// makes a resource that was created while out of memory invalid for good, so every later
/// use of it fails validation and would otherwise hide the allocation that failed.
///
/// Upstream commit 97b37768 keeps this on its `Gpu` type; this fork keeps the device in
/// `GpuState` instead, so it is a free function over the device.
fn checked<T>(device: &wgpu::Device, work: impl FnOnce() -> Result<T>) -> Result<T> {
    let memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let result = work();
    let invalid = pollster::block_on(validation.pop());
    let short = pollster::block_on(memory.pop());
    match short.or(invalid) {
        Some(wgpu::Error::OutOfMemory { source }) => {
            Err(anyhow::Error::new(OutOfMemory(source.to_string())))
        }
        Some(error) => Err(anyhow!("the GPU failed: {error}")),
        None => result,
    }
}

pub(crate) fn rgba([r, g, b, a]: [u8; 4]) -> Color {
    Color::from_rgba8(r, g, b, a)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WGPU's default limits cap textures at 8192 px, far below what desktop
    /// GPUs support, and tall scans or spreads exceed it.
    ///
    /// Ignored in environments whose WGPU adapter self-reports a
    /// `max_texture_dimension_2d` below 9000 (e.g. software Vulkan with an
    /// 8192 limit): the rasterizer correctly refuses such surfaces there, so
    /// the test cannot pass without a higher-limit adapter.
    #[test]
    #[ignore = "requires a WGPU adapter with max_texture_dimension_2d >= 9000"]
    fn rasterizes_surfaces_beyond_the_default_wgpu_texture_limit() {
        let image = Rasterizer::new()
            .unwrap()
            .rasterize_scene(
                &Scene::new(),
                4,
                9000,
                [10, 20, 30, 255],
                RasterOptions::default(),
            )
            .unwrap();
        assert_eq!(image.dimensions(), (4, 9000));
        assert_eq!(image.get_pixel(3, 8999).0, [10, 20, 30, 255]);
    }

    /// Upstream commits fbe36bd9 and 5f0fd18c bounded the GPU-side renderer memory by
    /// bytes. The pool here bounds both how many targets it keeps and what they hold, and
    /// it returns the least recently used one first.
    #[test]
    #[ignore = "requires a WGPU adapter"]
    fn the_render_target_pool_stays_within_its_count_and_byte_bounds() {
        let (device, _queue) = pollster::block_on(request_device()).unwrap();
        let mut pool = TargetPool::default();
        for _ in 0..8 {
            pool.give_back(RenderTarget::new(&device, 512, 512).unwrap());
        }
        assert_eq!(pool.targets.len(), MAX_POOLED_TARGETS);
        assert!(pool.retained_bytes() <= MAX_POOLED_TARGET_BYTES);

        // A target heavier than the whole budget is not pooled at all.
        let oversized = RenderTarget::new(&device, 8192, 8192).unwrap();
        assert!(oversized.byte_len() > MAX_POOLED_TARGET_BYTES);
        pool.give_back(oversized);
        assert_eq!(pool.targets.len(), MAX_POOLED_TARGETS);
        assert!(pool.retained_bytes() <= MAX_POOLED_TARGET_BYTES);
    }
}
