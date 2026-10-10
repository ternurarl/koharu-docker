# Golden raster baseline manifest

这些 PNG 产自**修复前**的未修改实现(vello 0.9.0,图像图集放不下时静默跳过绘制的版本),
是修复后"新旧光栅化逐像素等价性"比对的唯一基准。

- 采集测试:`../golden_capture.rs`,全部标注 `#[ignore]`(防止普通 `cargo test` 用新实现覆盖基线)。
- **唯一允许的生成命令**(在仓库根目录执行):

  ```text
  PATH=/tmp/fakebin:$PATH cargo test -p koharu-renderer --test golden_capture -- --ignored
  ```

- 采集参数:`Renderer::render` + `Renderer::rasterize(RasterOptions::default())`
  (supersampling_factor=1,即 1:1 无缩放;downsample_filter=Lanczos3 未参与)。
- 光栅背景为 `[0,0,0,0]`(见 `raster.rs` `rasterize`),页面 source 全页不透明,因此合成结果 alpha 均为 255。
- 生成时间:2026-09-22(本机,软件 wgpu 渲染,EGL 权限告警可忽略)。

## G1 — `g1_source_cleanup_1x1.png` (64×96)

构造:
- 页 64×96;source 整页纯色 `[10,20,200,255]`(PNG 编码)。
- 子实体 `Cleanup`(RasterLayer,`kind=Cleanup`,插在 `At::Start`,先于其他子实体绘制),
  整页半透明 `[240,30,20,128]`,像素 `(10,10)` 处不透明 `[200,100,50,255]`。
- 无 Visibility 组件(默认 opacity=1.0),1:1 无缩放。

抽样像素(从生成 PNG 读回):

| (x,y) | RGBA | 说明 |
|---|---|---|
| (0,0) | `[125,25,110,255]` | cleanup 半透明覆盖 source 的 src-over 合成 |
| (10,10) | `[200,100,50,255]` | 不透明点原样 |
| (63,95) | `[125,25,110,255]` | 右下角同合成值 |

## G2 — `g2_cleanup_opacity.png` (64×96)

构造:同 G1,但 Cleanup 实体额外携带
`Visibility { origin: User, visible: true, opacity: 0.5 }`
(renderer.rs 遍历中 `opacity: inherited.opacity * visibility.opacity`,最终在
`frame.rs::append_with_presentation` 里以 `push_layer(..., opacity, ...)` 施加)。

抽样像素:

| (x,y) | RGBA | 说明 |
|---|---|---|
| (0,0) | `[68,23,155,255]` | cleanup 有效 alpha 128×0.5=64 再覆盖 source |
| (10,10) | `[105,60,125,255]` | 不透明点整体 0.5 不透明度覆盖 source |
| (63,95) | `[68,23,155,255]` | 同 (0,0) |

对比 G1 可验证 opacity=0.5 的语义(G1/G2 逐点不同)。

## G3 — `g3_embedded_scaled.png` (128×128)

构造:
- 页 128×128;source 为 128×128 棋盘格 PNG:4×4 格网、每格 32 px,
  格 `(cx+cy)` 为偶数 `[20,20,20,255]`(深)、奇数 `[235,235,235,255]`(浅)。
- Embedded 图像层:裸子实体(非 RasterLayer/TextLayout/Region,故走 renderer.rs:728-750
  的 Geometry+asset 分支,`ImageKind::Embedded`),`Geometry::rectangle(32,32,64,64)`,
  asset 为 16×16 渐变 PNG:`[x*16, y*16, 128, 255]`(x,y ∈ [0,16)),
  即 16×16 拉伸到 64×64 → 4× 缩放采样。

抽样像素:

| (x,y) | RGBA | 说明 |
|---|---|---|
| (0,0) | `[20,20,20,255]` | 深色棋盘格 |
| (31,31) | `[20,20,20,255]` | 嵌入矩形外的深色格 |
| (33,33) | `[0,0,128,255]` | 嵌入渐变左上角(≈源 (0,0)) |
| (64,64) | `[122,122,128,255]` | 嵌入渐变中部缩放采样 |
| (95,95) | `[240,240,128,255]` | 嵌入渐变右下附近(≈源 (15,15)) |
| (127,127) | `[20,20,20,255]` | 深色棋盘格 |

## G4 — `g4_text_over_images.png` (64×96)

构造:
- source + Cleanup 同 G1。
- 文字实体:`add_text_content`(SourceText `原文` + Translation `你好世界`),
  `add_text_layer`(`SceneTextLayout { kind: Paragraph }`,经 Presents 关系挂到 content,
  处于页面 TextGroup 下),`Geometry::rectangle(8,8,48,40)`,
  `Typography { size: 20, auto_fit: false, color: [0,0,0,255], alignment: Start, writing_mode: Horizontal }`。
- 字体来自系统 fontique 发现(fallback 链,本机含 Droid Sans Fallback 等 CJK 字体)。

抽样像素:

| (x,y) | RGBA | 说明 |
|---|---|---|
| (0,0) | `[125,25,110,255]` | 背景合成(同 G1) |
| (10,10) | `[200,100,50,255]` | cleanup 不透明点(文字框外) |
| (18,7) | `[15,3,13,255]` | 文字笔画(近黑,抗锯齿边缘) |
| (63,95) | `[125,25,110,255]` | 背景合成 |

整个 G4 光栅中近黑(R,G,B 均 < 40)像素共 341 个,非背景像素共 776 个——
文字确实绘制在图像层之上。文字渲染依赖系统字体发现,跨机器复现时字体度量可能不同;
等价性比对应在同一台机器上进行(修复不改文字渲染路径)。

## 体积

4 个 PNG 合计约 4.3 KB(golden 目录总共 2585+1038+447+446 字节)。
