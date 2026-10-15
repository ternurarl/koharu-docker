# 渲染导出"丢背景"问题报告与修复交接

- 日期:2026-09-22
- 状态:根因已定位并本地复现,**修复未实施**(本文件即交付给执行修复的 agent 的完整上下文)
- 范围:`koharu-docker`(本仓库)渲染导出链路;`../astrbot_plugin_koharu`(调用方)只做取证,不在修复范围内
- 结论一句话:**翻译数据没有丢,是渲染导出时 vello 0.9.0 的图像图集(atlas)放不下全页大图时会静默跳过该图的绘制(源码注释明言 "don't attempt to render anything"),导致导出 PNG 只剩译文文字、背景全透明;插件随后的 Pillow WEBP 转码忠实保留了透明底。**

---

## 0. 执行者的任务与验收(先读这节)

**任务**:修复渲染导出丢图像层的缺陷(根因见第 3 节,修复选项见第 5 节,推荐选项 A)。验收标准见第 6 节。第 4 节列出已排除的路径,**不要重查**。附录 B 有可直接运行的复现工程完整源码。

---

## 1. 系统背景(接手必读)

### 1.1 涉及的仓库

| 仓库 | 职责 |
|---|---|
| `koharu-docker`(本仓库) | Koharu 漫画翻译引擎 fork。Rust workspace;`--features headless` 编译出无 GUI 的 REST 服务(Docker 部署,长驻进程,`ENTRYPOINT koharu --headless`)。翻译管线 + 场景模型 + 渲染导出都在这里。 |
| `../astrbot_plugin_koharu` | AstrBot(QQ 机器人)插件。调用上述 REST API 完成"发图→翻译→收图"。 |

### 1.2 一次翻译请求的完整链路

```
QQ 用户发图
  → 插件 main.py::_translate_images(../astrbot_plugin_koharu/main.py:561)
      POST /projects/{name}            建项目(每次请求一个新项目,导出后删除)
      POST /projects/current/pages     multipart 上传图片(每图一页)
      POST /pipelines                  {"operation":{"operation":"full"},"scope":{"scope":"project"}}
      GET  /operations/{id}            轮询到 completed
      POST /projects/current/export    {"format":"rendered"}        ← ★ 问题产物在这里生成
  → 服务端 crates/koharu-rpc/src/routes/projects.rs:201-220:
      renderer.render(snapshot, page) → renderer.rasterize(frame) → PNG 字节
  → 插件 main.py::_compress_image(main.py:1198):
      Pillow image.save(target, "WEBP", quality=…, method=6)        ← 保留 alpha,不做合成
  → 发回 QQ(默认 return_image_format="webp")
```

关键事实:**webp 只是导出 PNG 的无损语义转存**。插件里已有"成品守卫"(main.py:824 `_guard_rendered_export`、main.py:1298 `_render_guard_issues`)在导出后检测"透明占比 > 5%"并重试一次——这说明症状产自服务端导出 PNG,插件侧只是兜底(CHANGELOG v1.6.7 称之为"渲染缺背景层(08-14 事故根因)")。

### 1.3 渲染架构(与 bug 相关的部分)

```
Snapshot(场景:页面 + source 图资产 + Cleanup 修复层资产 + 文字实体)
  → Renderer::render      crates/koharu-renderer/src/renderer.rs:116
      compile():遍历场景 → 每个图层/文字节点 → LayerDraft
        · 页面 source 图 = 无条件全页图层(renderer.rs:558)
        · RasterLayer(Cleanup/Paint)子实体 = 全页图层(修复层,inpainting 写入的透明底 PNG)
        · 文字节点 = 只画字形(+可选描边);气泡白底**不**由文字层画,来自背景层
      finish():逐节点 build_node → vello Scene(图像节点 = scene.draw_image)
  → Renderer::rasterize   crates/koharu-renderer/src/raster.rs:133
      把全部图层按序 append 成一个 vello Scene(raster.rs:141 以透明底 [0,0,0,0] 渲染)
      → GPU(vello 0.9.0 + wgpu 29)→ 读回 RGBA → RgbaImage
```

**单例渲染器**:`koharu-rpc/src/state.rs:75` 的 `App::new` 创建**一个** `koharu_renderer::Renderer`,进程级共享;其内部 `rasterizer` 是 `OnceCell` 单例(renderer.rs:65),其中又持有**唯一一个** `vello::Renderer`(创建于 raster.rs:110-131)。而 vello 的 `Renderer` 内部持有 `resolver: Resolver`(vello-0.9.0/src/lib.rs:332、448)——**Resolver 内含图像图集缓存,跨所有渲染调用持久累积**。这就是 bug 能跨越"每请求一个项目"边界累积的原因。

多页导出是一次 HTTP 调用内的顺序循环(projects.rs:193-225),每页 `render + rasterize`,共用上述单例。批量翻译(合并转发/引用多图,插件一次最多 20 张)就会连续渲染 20 页。

---

## 2. 问题症状与取证

### 2.1 症状

`待翻译.jpg`(2894×4093 灰度漫画页)翻译后得到 `翻译结果.webp`,**背景完全丢失,只剩透明底上的中文译文文字**("恋爱谜题的话就交给我!"等,竖排,位置与各气泡对应正确)。

### 2.2 像素取证(对 翻译结果.webp 解码 RGBA 后统计)

| 指标 | 数值 | 说明 |
|---|---|---|
| 尺寸 | 2894×4093 | 与源图一致,全尺寸导出 |
| alpha=0(全透明) | 98.49% | 背景/修复层完全没有画上来 |
| alpha=255 | 1.09% | 全部为字形实心 |
| 半透明 | 0.42% | 字形抗锯齿边 |
| 可见像素颜色 | 100% 纯黑 (0,0,0) | 只有文字填充色;无白色气泡底、无描边 |
| 文字 bbox | (88,345)-(2772,3540) | 散布整页、与气泡位置吻合 = 完整的页面坐标文字层合成 |

注:文字层**本来**就只画字形(气泡白底设计上来自背景层),所以"纯黑字形 + 其余全透明"正是"图层列表里只剩文字层"的精确特征。

### 2.3 产出方判定(编码指纹)

- RIFF 结构:`VP8X + ALPH(header 0x01, compression=1) + VP8 ` = **libwebp 有损 + alpha**。
- 对照实验:ffmpeg `libwebp -lossless 0` 产出同构文件;`image` crate 0.25.10 的 WebP 保存器只支持 **VP8L 无损**(docs.rs 源码明言 "Right now only lossless encoding is supported"),其 VP8L 结构与实物不符 → **排除 `crates/koharu-pipeline/src/bin/run.rs:203-208` 的 `image.save()` 路径**。
- 仓库内所有 `webp::Encoder::from_rgba().encode(q)` 调用点只产出 ≤1024px 的缩略/预览 → 排除。
- 结合 2.1 节链路,产物 = 插件 Pillow `"WEBP"` 转码(main.py:1218-1226)的输出,其输入即服务端导出 PNG。

### 2.4 历史症状变体

2026-08-14 事故(插件 CHANGELOG v1.6.7)记作"**白底**只有文字":同一个 bug,只是当输出格式为 jpg 时插件 `_prepare_jpeg_image`(main.py:1228)把透明底拍平成白底,或在白色查看器里看透明 PNG。默认 webp 后呈现为"透明图层的翻译文字"。两种呈现是同一根因。

---

## 3. 根因(核心)

### 3.1 vello 0.9.0 图像图集的行为

vello(Cargo.lock 锁定 vello 0.9.0 / vello_encoding 0.9.0)把场景里**所有图像**装进一个正方形"图集"纹理(guillotiere 矩形打包):

```rust
// vello_encoding-0.9.0/src/image_cache.rs:9-11
const DEFAULT_ATLAS_SIZE: i32 = 1024;
const MAX_ATLAS_SIZE: i32 = 8192;       // 扩容 1024→2048→4096→8192,到顶为止
const EVICT_AFTER_GENERATIONS: u64 = 2; // 驻留 3 代之后才允许逐出
```

生命周期要点(全部在 `image_cache.rs` / `resolve.rs:505-539` `resolve_pending_images`):

1. 驻留表 `map: HashMap<BlobId, ResidentImage>` **跨渲染持久**(挂在 `Renderer.resolver` 上),按"代(generation)"记录最后使用时间;只有当**分配失败时**才做逐出扫描(`evict_stale_entries`),逐出条件是 `last_used_generation < generation-2`(即驻留 3 代才过期),且**每个 resolve 只允许一次逐出扫描**(内部 guard `if self.evicted_in_resolve != 0 { return false; }`)。
2. 分配失败时依次尝试:逐出过期驻留 → 图集倍增扩容(`bump_size`,到 8192² 为止)→ 都不行就放弃该图。
3. **放弃的行为是静默跳过绘制** —— `resolve.rs:300-311`:

```rust
// If we get here, we failed to allocate a slot for this image in the atlas.
// In this case, let's zero out the dimensions so we don't attempt to render
// anything.
// TODO: a better strategy: texture array? downsample large images?
data.extend_from_slice(&[0_u8; 8]);
```

即把该图的绘制数据尺寸清零,**不画、不报错、不打日志**。上游自己在 TODO 里承认对大图的处理是待改进项。

### 3.2 几何数学:8192² 图集对本例只能放 4 张全页图

本例的 source 图与 Cleanup 修复层都是 2894×4093:

- 2×2894 = 5788 ≤ 8192,3×2894 = 8682 > 8192 → 一行最多 **2** 张;
- 2×4093 = 8186 ≤ 8192,3×4093 = 12279 > 8192 → 一列最多 **2** 张;
- → **2×2 = 4 张封顶**,第 5 张 2894×4093 无论怎样都放不下(剩余边角条带宽度 404px)。

每导出一页要放 **2** 张(source + Cleanup;inpainting 正常运行时。若只有 source 则 1 张):

| 批量导出的页序(每页 2 图) | 图集占用 | 结果 |
|---|---|---|
| 第 1-2 页 | 4 张满 | 正常 |
| 第 3 页 | 两张都分配失败;此时第 1-2 页驻留未满 3 代不可逐出;扩容已到 8192² 顶 | **两张全被静默丢弃 → 纯透明/纯文字页**(复现实测) |
| 第 4 页 | 逐出扫描可逐出第 1 页的 2 张(刚好满 3 代),腾出 2 格;但"每 resolve 一次扫描"的限制让第 2 张图常抢不到 | 恢复或只丢修复层(震荡) |
| 第 5-6 页 | 同类时序 | 继续震荡/再全丢(复现实测第 6 页全丢) |

**字形不受影响**:glyph 走独立的 `glyph_cache.rs`,文字照常画出 → 输出恰为"透明底 + 译文文字"。

### 3.3 koharu 的使用方式为何踩满条件

| 条件 | koharu 侧事实 |
|---|---|
| 图集跨渲染累积 | 进程级唯一 `vello::Renderer`(state.rs:75、renderer.rs:65、raster.rs:110-131),服务是长驻容器,跨请求/跨项目永不重建 |
| 每页塞大图 | source 是全页原图(renderer.rs:558,尺寸=页尺寸);Cleanup 修复层是全页透明底 PNG(inpainting.rs:329-352);本例 2894×4093 扫图属于最坏档位 |
| 连续渲染多页 | `export` 多页是顺序循环(projects.rs:193-225),批量翻译一次最多 20 页 |
| 无感知失败 | koharu 的 `rasterize` 不校验结果像素;vello 侧零报错;`frame.diagnostics()` 只含 koharu 自己的诊断,导出路径也不读 |

### 3.4 观察到的每个现象 ← 机制解释

| 现象 | 解释 |
|---|---|
| 批量多图时第 3 张起"缺背景层" | 2×2 图集几何上限(3.2 节) |
| 单图请求**紧跟大请求之后**也中招 | 上一请求最后几页的图仍驻留(要 3 代才过期),本次 source/修复层分配失败;而本次的逐出扫描可能已用完单次配额 |
| 缺失程度震荡(全丢 / 只丢修复层 / 恢复) | 逐出"3 代过期 + 每 resolve 一次扫描"的时序(3.2 节表) |
| 插件守卫"重试导出有时恢复" | 重试=再渲染一次=推进一代 → 旧驻留过期可逐出 → 背景重新放回图集 |
| 重试"仍异常"时有发生 | 重试那一代的驻留仍占满 2×2,或两张图在同一 resolve 里争抢单次逐出配额 |
| 缩略图(128px)/预览(1024px)从不出问题 | 1024² 起步图集就放得下几十张小图 |
| 只有文字、没有气泡白底 | 文字节点只画字形(renderer.rs:869 默认前景黑、无描边),气泡底本应来自背景层 |
| 输出可见像素全是纯黑 | 同上:字形填充色即 inferred 文字色(黑) |
| 08-14 "白底只有文字" | 同根因,JPEG 白底拍平的呈现变体(2.4 节) |
| 仓库测试从未发现 | 集成测试最大 4×4(rendering.rs:99),bench 512×768(benches/rendering.rs:43-59),从无"同一渲染器连续渲染多张全页大图"的场景 |
| 与显卡/驱动无关 | 复现机 GPU 节点无权限、EGL 回落软件渲染(lavapipe)仍 100% 复现 —— 纯分配逻辑问题 |

### 3.5 复现实验(实测,2026-09-22)

独立工程(源码见附录 B,原位于 `/tmp/koharu_repro`,清理后按附录重建):用 koharu 真实的 `Renderer::render + rasterize`,单共享渲染器,顺序导出 6 页,每页 2 张 2894×4093 图层(source 红色不透明 + Cleanup 绿色半透明),与导出路径逐字一致:

```
export page 1: layers=2 visible_px=100.0% center=[125,125,30,255] => OK (background rendered)
export page 2: layers=2 visible_px=100.0% center=[125,125,30,255] => OK (background rendered)
export page 3: layers=2 visible_px=  0.0% center=[0,0,0,0]        => LOST BACKGROUND (text-only/transparent)
export page 4: layers=2 visible_px=100.0% center=[125,125,30,255] => OK (background rendered)
export page 5: layers=2 visible_px=100.0% center=[125,125,30,255] => OK (background rendered)
export page 6: layers=2 visible_px=  0.0% center=[0,0,0,0]        => LOST BACKGROUND (text-only/transparent)
```

**读法:每行 `layers=2` 证明 koharu 的帧里图层数据始终完整 —— 丢失发生在 vello 图集分配环节**;第 3、6 页两张图像层被静默丢弃(生产环境同一页上还有文字层,于是呈现为"透明底+文字")。修好后 6 页应全部 OK。

---

## 4. 已排除的路径(不要重查)

1. **场景/资产数据层** —— 审计结论:干净。
   - 管线运行中 OCR/Inpainting 都强制读取页面 source 图(ocr.rs:95 起、inpainting.rs:420 起,"page has no source image" 即报错),译文能产出证明 source 资产当时完好;
   - `set_asset`/`remove_asset`/`Operation::ReplaceComponent` 全带 OCC 冲突校验(patch.rs:164-178),冲突=任务 failed,插件会报错而不会出成品;
   - blob 存储是"租约 + 引用闭包"(koharu-storage/src/blobs.rs、session.rs `update`/`save`),引用缺失是硬错误("asset blob is missing",components/assets.rs:52-56),不会静默返回 None;组件序列化有 `ReferenceMismatch` 完整性校验(component.rs:181-205);
   - `snapshot.asset()` 静默返回 None 是渲染器唯一静默丢图层的入口(renderer.rs:558、668-697),但资产存在已被上条证明;复现也证实 `layers=2`。
2. **阶段并发提交** —— `patch.rebase_on` 的 observation 校验(translation/inpainting 并发场景)不会静默丢数据,冲突直接让任务 failed;且检测/OCR/翻译的实体结构互不冲突(已核对 scheduler.rs:145-151 的依赖关系与各阶段 edit 调用)。
3. **插件侧** —— `_compress_image` 只做格式转码;守卫逻辑只检测与重试,不改像素。
4. **其它 webp 产出路径** —— 见 2.3 节编码指纹排除法。
5. **GPU/驱动兼容性** —— 软件渲染(lavapipe)同样复现(3.5 节)。

---

## 5. 修复选项(供执行者决策;推荐 A)

### 选项 A(推荐):图像层在 CPU 合成,vello 只画文字/矢量 —— 彻底绕开图集

设计:`Rasterizer::rasterize`(raster.rs:133)按层类型拆分:

1. 图像层(Kind=Image:Source/Cleanup/Paint/Embedded)不进 vello 场景,用 `image` crate 按层序合成:source 垫底 → Cleanup 覆盖(`image::imageops::overlay` 天然处理 alpha)→ 其它嵌入图;每层应用 `Layer::append_with_presentation` 同款语义(placement Affine 的平移+缩放、visibility、opacity)。注意 `image_draft` 的图像层 `angle_degrees` 恒为 0(renderer.rs:1113-1118),只需 scale+translate,无旋转分支。
2. 文字/矢量层照旧 vello 渲染(透明底),产物整体 alpha 合成到图像层之上。
3. 无文字层的页可完全跳过 vello;无图像层的页维持现状。

理由:
- 文字场景不含 `draw_image`,**完全不触碰图集**,分配失败问题从根上消失;
- 不 fork vello、不赌上游行为,跨平台确定;
- 天然同时修掉同类隐患:**PSD 导出的逐层光栅化**(koharu-psd/src/document.rs:93 合并渲染 + :157 逐层渲染)在多层大图页上同样会踩图集上限——选项 A 若落在 Rasterizer 层则一并覆盖(执行者请确认 psd 路径;图像层的逐层像素本就可以直接用 decoded 原像素,不必走 GPU)。

性能实测(2026-09-22,2894×4093=11.8 MPx/45.2 MiB 每层;vello 侧为软件渲染,真实 GPU 只会更快):

| 操作 | 单线程耗时 |
|---|---|
| `image::imageops::overlay`(全像素半透明,最坏情况) | 208 ms —— **不要用这个原语,慢 4.4 倍** |
| 手写 src-over 循环(同最坏情况) | 47.5 ms |
| 现状 vello render+rasterize 每页(含读回) | ~150 ms(软件渲染;首帧 616 ms 含初始化) |

结论:选项 A 每页新增约 20~50 ms(真实图层稀疏:Cleanup/文字栅格大多 alpha=0 走跳过分支,实际每层 5~20 ms),同时省掉每页 2×45 MiB 的 GPU 纹理上传;相对 ML 管线的每页几十秒可忽略。如需进一步压:复用 `renderer.rs::workers()` 的 rayon 池按行并行(~10 ms 级)。注意 `supersampled(factor>1)` 时像素按 factor² 放大,但当前所有导出路径均为 factor=1,且 GPU 路径同样按倍率付出代价,不构成反选理由。

风险/边界(验收时覆盖):
- `RasterOptions::supersampled(factor)`(raster.rs:46-53)开启时,图像层需按同一倍率重采样(`fast_image_resize` 已是依赖);
- Embedded 图层的缩放质量、半透明 Cleanup 与 source 的合成顺序必须与 vello 版逐像素等价(可用现有测试 `rendering.rs::rasterized_png_contains_source_and_cleanup_pixels` 比对);
- `Raster { left, top }` 偏移语义保持不变(raster.rs:74-78)。

### 选项 B:给 vello 打补丁(`[patch.crates-io]` fork vello_encoding)

- 治本方向是上游 TODO 所述:分配失败时降级为"独立纹理直绘"(涉及 shader 绘制数据布局,工作量大);或图集支持非方形/更大(8192×16384 也只是把 4 张变 8 张,20 页批量仍不够;更大纹理在 iGPU 上内存不现实);
- 代价:维护 fork,vello 升级需 rebase;且 3.2 节的驻留/逐出时序 bug(单次扫描配额)也需要一并修才不震荡。
- 不推荐作为首选;若选 B,至少同时修"分配失败静默"与"每 resolve 单次逐出扫描"两点。

### 选项 C(缓解,不能治本,可作辅助)

- 导出批次前重建 `Renderer`/`Resolver`:只解决**跨请求**驻留污染,**批次内**第 3 页依旧丢;
- 插件守卫 + 重试(已存在)只是概率缓解,且把压力转给调用方。
- 不建议单独采用。

### 重要踩坑警告(已核实 vello 0.9 API 文档源码)

`Renderer::override_image` / `register_texture`(lib.rs:536-604)**不能**绕过图集:文档明言数据仍会"copied into the slot in Vello's image atlas where the image would be placed"——只替换图集槽位的数据源,**分配不到槽位时照样被清零跳过**。不要把它们当作修复手段。

---

## 6. 验收标准

1. **复现工程全绿**:按附录 B 重建并运行,6 页全部 `100.0% => OK`。
2. **回归不破**:`cargo test -p koharu-renderer -p koharu-pipeline -p koharu-psd` 全过(在 Dockerfile 的 Ubuntu builder 环境里跑,依赖见 HEADLESS.md;或本机按附录 B.3 的构建坑位处理)。
3. **新增回归测试**(落在 `crates/koharu-renderer/tests/rendering.rs`):同一 `Renderer` 顺序渲染 ≥5 页、每页 ≥2 张 ≥2000×3000 图层,断言各页抽样像素不透明且等于预期色。软件渲染(lavapipe)即可跑,无需 GPU。
4. **端到端验证**:起 headless 服务,一次 `POST /projects/current/pages` 上传 ≥6 张大图 → `POST /pipelines` full → `POST /projects/current/export` {"format":"rendered"},解包 zip 断言每页 PNG 透明占比 ≈ 0(可用附录 C 的守卫算法)。
5. **等价性**:若采用选项 A,需证明新旧光栅化对"source+Cleanup+文字"页面逐像素一致(容差 ≤ 抗锯齿边缘);现有测试 `rendering.rs::rasterized_png_contains_source_and_cleanup_pixels` 的断言是最小集。
6. 文档:修复后在本文件追加"已修复于 commit xxx + 选项 X",便于追溯。

## 7. 约束与注意事项

- **不要修改 `../astrbot_plugin_koharu`**(本次范围外;其守卫可在服务端修复稳定后另行简化)。
- 修复应保持 REST 契约、`Raster`/`Frame` 公开 API 与 PSD 导出格式不变;`RasterOptions` 可增字段但需默认值兼容。
- 性能注意:选项 A 的 CPU 合成对 2894×4093 是纯 memcpy 级开销,远小于当前 GPU 往返;勿引入额外全尺寸往返拷贝超过一次。
- 本文件所在仓库的 `git status` 中有用户自己的在制品(AGENTS.md 修改、UPSTREAM_SYNC_PLAN.md、两个中文名图片),**保持原样,不要清理或提交**。
- 复现工程在 `/tmp/koharu_repro`(重启可能被清理);完整源码在附录 B,可随时重建。

---

## 附录 A:关键代码位置索引

vello(Cargo.lock:vello 0.9.0 / vello_encoding 0.9.0,位于 `~/.cargo/registry/src/index.crates.io-*/`):

| 位置 | 内容 |
|---|---|
| `vello_encoding-0.9.0/src/image_cache.rs:9-11` | 图集常量(1024/8192/逐出 2 代) |
| `vello_encoding-0.9.0/src/image_cache.rs:76-80` | `begin_resolve`:每 resolve 一代,清上传表 |
| `vello_encoding-0.9.0/src/image_cache.rs:82-90` | `restart_resolve_pass`:扩容重启时回拨使用戳 |
| `vello_encoding-0.9.0/src/image_cache.rs:101-111` | `bump_size`:倍增扩容到 max_size |
| `vello_encoding-0.9.0/src/image_cache.rs:113-146` | `get_or_insert`:按 `Blob.id()` 驻留,分配失败返回 None |
| `vello_encoding-0.9.0/src/image_cache.rs:162-165` | `can_fit_image`:逐维比较当前图集边长 |
| `vello_encoding-0.9.0/src/image_cache.rs:167-183` | `evict_stale_entries`:**单次扫描 guard** + 3 代过期 |
| `vello_encoding-0.9.0/src/image_cache.rs:184-205` | `repack_to_size`:扩容重排,任一驻留放不下则整体失败 |
| `vello_encoding-0.9.0/src/resolve.rs:292-311` | **静默丢弃**:`xy=None` → 维度清零 "don't attempt to render anything" |
| `vello_encoding-0.9.0/src/resolve.rs:505-539` | `resolve_pending_images`:分配→逐出→扩容→放弃 的完整决策环 |
| `vello-0.9.0/src/lib.rs:332/448` | `Renderer.resolver: Resolver` 跨渲染持久 |
| `vello-0.9.0/src/lib.rs:536-604` | `override_image`/`register_texture`/`unregister_texture`:**仍走图集槽位**,不能作为修复(见第 5 节警告) |
| `vello_encoding-0.9.0/src/glyph_cache.rs` | 字形独立缓存(解释"文字没事") |

koharu(本仓库):

| 位置 | 内容 |
|---|---|
| `crates/koharu-rpc/src/state.rs:75` | 进程级唯一 `Renderer` |
| `crates/koharu-renderer/src/renderer.rs:65` | `rasterizer: OnceCell`(唯一 vello::Renderer) |
| `crates/koharu-renderer/src/renderer.rs:116-119、531-608` | `render`/`compile`:source 图层无条件入帧(558) |
| `crates/koharu-renderer/src/renderer.rs:662-699` | RasterLayer(Cleanup/Paint)图层入帧 |
| `crates/koharu-renderer/src/renderer.rs:860-869` | 文字节点只画字形,默认黑色、无描边 |
| `crates/koharu-renderer/src/raster.rs:133-144` | `rasterize`:透明底合成全帧(**修复落点**) |
| `crates/koharu-renderer/src/raster.rs:46-53` | `RasterOptions::supersampled` |
| `crates/koharu-renderer/src/frame.rs:287-322` | `append_with_presentation`:图层 visibility/opacity/placement 语义 |
| `crates/koharu-rpc/src/routes/projects.rs:201-220` | REST 导出 render+rasterize → PNG(**问题产物**) |
| `crates/koharu-pipeline/src/stages/inpainting.rs:329-352` | Cleanup 修复层资产写入(全页透明底 PNG) |
| `crates/koharu-pipeline/src/stages/ocr.rs:95-97`、`inpainting.rs:420-421` | source 图缺失即报错(数据层完好的反证) |
| `crates/koharu-psd/src/document.rs:93/157` | PSD 合并+逐层光栅化(同类隐患) |
| `crates/koharu-pipeline/src/bin/run.rs:203-208` | `image.save()` 输出路径(已由编码指纹排除) |

插件(取证用,`../astrbot_plugin_koharu`):

| 位置 | 内容 |
|---|---|
| `main.py:561` `_translate_images` | 全流程编排 |
| `main.py:824` `_guard_rendered_export` / `main.py:1298` `_render_guard_issues` | 成品守卫(透明>5% 硬信号、白占比软信号) |
| `main.py:1198` `_compress_image`(WEBP 于 :1218-1226) | 产出 翻译结果.webp 的转码 |
| `main.py:1228` `_prepare_jpeg_image` | JPEG 白底拍平(08-14 "白底只有文字"变体的来源) |
| `CHANGELOG.md` v1.6.7 | 守卫上线与"08-14 事故根因"记述 |

## 附录 B:复现工程(完整源码与运行方法)

### B.1 `/tmp/koharu_repro/Cargo.toml`

```toml
[package]
name = "koharu_repro"
version = "0.0.0"
edition = "2021"

[dependencies]
koharu-renderer = { path = "/home/qinzhu/koharu/koharu-docker/crates/koharu-renderer" }
koharu-scene = { path = "/home/qinzhu/koharu/koharu-docker/crates/koharu-scene" }
image = "0.25.10"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
anyhow = "1"

[workspace]
```

### B.2 `/tmp/koharu_repro/src/main.rs`

```rust
use std::{collections::BTreeMap, io::Cursor, sync::Arc};

use anyhow::Result;
use koharu_renderer::{RasterOptions, Renderer};
use koharu_scene::{
    AssetInput, AssetMetadata, AssetRole, At, PageDraft, RasterLayer, RasterLayerKind, Origin, Session,
};

fn solid_png(width: u32, height: u32, color: [u8; 4]) -> Arc<[u8]> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba(color));
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner().into()
}

#[tokio::main]
async fn main() -> Result<()> {
    let (width, height) = (2894u32, 4093u32);
    let pages_in_batch = 6;
    let source = AssetRole::new("source")?;
    let mut session = Session::memory().await?;
    let mut page_ids = Vec::new();
    let patch = session.snapshot().patch(|edit| {
        for index in 0..pages_in_batch {
            let page = edit.add_page(
                PageDraft::new(format!("page{index}"), f64::from(width), f64::from(height)),
                At::End,
            )?;
            edit.set_asset(
                page,
                &source,
                AssetInput::new(
                    solid_png(width, height, [220, 30, 30, 255]),
                    "image/png",
                    AssetMetadata { width: Some(width), height: Some(height), attributes: BTreeMap::new() },
                ),
            )?;
            let cleanup = edit.add_entity(page, At::Start)?;
            edit.set(
                cleanup,
                &RasterLayer { origin: Origin::User, name: "Cleanup".into(), kind: RasterLayerKind::Cleanup },
            )?;
            edit.set_asset(
                cleanup,
                &source,
                AssetInput::new(
                    solid_png(width, height, [30, 220, 30, 128]),
                    "image/png",
                    AssetMetadata { width: Some(width), height: Some(height), attributes: BTreeMap::new() },
                ),
            )?;
            page_ids.push(page);
        }
        Ok(())
    })?;
    let snapshot = session.commit(patch).await?.snapshot;

    // One shared renderer models the long-lived server process.
    let renderer = Renderer::new()?;
    println!("batch of {pages_in_batch} pages, each 2 images of {width}x{height}; one shared Renderer");
    for (index, page) in page_ids.iter().enumerate() {
        let frame = renderer.render(&snapshot, *page).await?;
        let raster = renderer.rasterize(&frame, RasterOptions::default()).await?;
        let visible = raster.image.pixels().filter(|p| p.0[3] > 0).count();
        let px = raster.image.get_pixel(width / 2, height / 2).0;
        let expected = width as usize * height as usize;
        println!(
            "export page {}: layers={} visible_px={} ({:.1}%) center={:?} => {}",
            index + 1,
            frame.layers().len(),
            visible,
            visible as f64 / expected as f64 * 100.0,
            px,
            if visible > expected / 2 { "OK (background rendered)" } else { "LOST BACKGROUND (text-only/transparent)" },
        );
    }
    Ok(())
}
```

### B.3 构建坑位(实测)

- **fontconfig**:`koharu-renderer`→`harfrust`→`fontique`→`yeslogic-fontconfig-sys` 构建期需要 pkg-config + fontconfig 开发包。在 Dockerfile 的 Ubuntu builder(装有 `libfontconfig-dev`)里无此问题;在无 root 的开发机上可用假 `pkg-config` shim 绕过(系统只要有 `libfontconfig.so.1` 运行库):

  ```bash
  mkdir -p /tmp/fakebin /tmp/fakelib
  ln -sf /usr/lib/x86_64-linux-gnu/libfontconfig.so.1 /tmp/fakelib/libfontconfig.so
  cat > /tmp/fakebin/pkg-config <<'EOF'
  #!/bin/bash
  for arg in "$@"; do
    case "$arg" in
      --modversion) echo "2.13.1"; exit 0;;
      --version) echo "0.29.2"; exit 0;;
    esac
  done
  case "$*" in
    *fontconfig*) echo "-L/tmp/fakelib -lfontconfig"; exit 0;;
    *) exit 1;;
  esac
  EOF
  chmod +x /tmp/fakebin/pkg-config
  ```

  注意:**不要**用 `RUST_FONTCONFIG_DLOPEN=1` 替代(会让 `fontique` 编译失败,E0432)。
- **GPU**:无需真 GPU;无 /dev/dri 权限时 wgpu 自动回落 lavapipe/软件渲染,复现照样成立(EGL 权限告警可忽略)。
- 运行:

  ```bash
  cd /tmp/koharu_repro
  PATH=/tmp/fakebin:$PATH cargo build --release
  ./target/release/koharu_repro
  ```

  失败特征(当前 bug 存在):第 3、6 页 `visible_px=0`。修复后应全部 100%。

## 附录 C:插件"成品守卫"算法(端到端验证可复用)

- 透明占比 = `1 - count(alpha>0)/total`(`_render_quality_stats`,main.py:1248);硬信号:> 5% 判异常。
- 白占比 = 铺白后 L≥241 像素占比;软信号:比源图基准高 0.30 以上判异常(纯白漫画页免疫误报)。
- 端到端验收(第 6 节第 4 条)只需硬信号:每页导出 PNG 透明占比应 ≈ 0。

## 附录 D:名词表

| 术语 | 含义 |
|---|---|
| 图集(atlas) | vello 把所有待画图像打包进的一张方形纹理;图像按矩形占位,采样时取图集坐标 |
| 驻留(resident) | 图像已在图集里占有槽位,以 `Blob.id()` 为键缓存,跨渲染复用 |
| 代(generation) | 每次 resolve +1 的计数,用于判定驻留的新旧;`EVICT_AFTER_GENERATIONS=2` 表示 3 代未用才可逐出 |
| resolve | vello 在每帧渲染前把场景里的图像补丁(位置、图集坐标)解析出来的过程;图集分配/逐出/扩容都发生在这一步 |
| source 层 | 页面的原始图片资产(角色名 `"source"`),渲染帧的最底层 |
| Cleanup/Paint 层 | `RasterLayer` 子实体资产;Cleanup 是 inpainting 产出的全页透明底修复图(只在改过的区域有像素) |
| 文字层 | `SceneTextLayout` + `Translation` 实体;渲染时只画字形(+可选描边),不含气泡底 |

## 修复记录

**已修复于 commit de9aae2dc73f2c7559d4c286572620eb50faf902 + 选项 A(主分支 main)。**

### 改动文件清单

- `crates/koharu-renderer/src/frame.rs`(修改:`Layer::decoded_image()`、`RetainedNode.pixels` 字段)
- `crates/koharu-renderer/src/lib.rs`(修改:新增 `mod compose;`)
- `crates/koharu-renderer/src/compose.rs`(新建:手写 straight-alpha src-over CPU 合成内核,opacity 乘入源 alpha、越界裁剪、最近邻缩放、1:1 整数平移逐像素精确)
- `crates/koharu-renderer/src/renderer.rs`(修改:两个 `RetainedNode` 构造点分别存入 `Arc<DecodedImage>`/`None`)
- `crates/koharu-renderer/src/raster.rs`(修改:`Rasterizer::rasterize` 图层编排:图像层 CPU 合成、文字段攒 Scene 由 vello 渲染后按层序 overlay、旋转/斜切图像层回退 vello)
- `crates/koharu-renderer/tests/rendering.rs`(修改:追加 `sequential_render_rasterize_keeps_image_layers_on_every_page` 回归测试)
- `crates/koharu-renderer/tests/golden_capture.rs`(新建:4 个 `#[ignore]` golden 基线采集测试)
- `crates/koharu-renderer/tests/golden_equivalence.rs`(新建:G1–G4 逐像素等价性测试,只读基线)
- `crates/koharu-renderer/tests/golden/`(新建:`g1_source_cleanup_1x1.png`、`g2_cleanup_opacity.png`、`g3_embedded_scaled.png`、`g4_text_over_images.png`、`manifest.md`)

### 六条验收标准逐条结论

1. **复现工程全绿 —— 通过**。按附录 B 重建 `/tmp/koharu_repro`(`PATH=/tmp/fakebin:$PATH cargo build --release` → `Finished 'release' profile … in 1m 57s`),运行后 6 页全部 `layers=2 visible_px=11845142 (100.0%) center=[125, 125, 30, 255] => OK (background rendered)`(11845142 = 2894×4093 全部像素);修复前第 3、6 页 `visible_px=0.0% LOST BACKGROUND` 的失败特征已消失。抽样 `center=[125,125,30,255]` 等于绿层 [30,220,30,128] 按 alpha≈0.502 叠红底 [220,30,30,255] 的预期混合。
2. **回归不破 —— 通过**。`cargo test -p koharu-renderer -p koharu-pipeline -p koharu-psd`(跑 2,`RUN2_EXIT:0`):pipeline lib 48 passed、run bin 1 passed、psd lib 17 passed、renderer lib 94 passed/2 ignored(既有 `#[ignore]` 字体基线)、golden_capture 0 passed/4 ignored(采集工具未运行)、golden_equivalence 4 passed(3.63s)、rendering 5 passed(17.80s)、doc-tests 0×3。(跑 1 曾因机器缺 `Noto Color Emoji` 字体致 2 个既有 shape 用例 `no fallback font available` 失败,`RUN1_EXIT:101`;安装该字体到 `~/.local/share/fonts` + `fc-cache` 后跑 2 全绿,未改任何 src。)
3. **新增回归测试 —— 通过**。`crates/koharu-renderer/tests/rendering.rs::sequential_render_rasterize_keeps_image_layers_on_every_page`:单 `Renderer` 顺序 render+rasterize 6 页(≥5),每页 Source+Cleanup 两层各 2000×3000,6×6 抽样点全部不透明且等于独立 src-over 预期(含原 bug 的第 3、6 页):`regression page 0..5: sampled [(0,0),(1999,0),(0,2999),(1999,2999),(1000,1500),(777,2222)] all opaque, expected [125,25,60,255]/[130,50,85,255]/[135,75,110,255]/[140,100,135,255]/[145,125,160,255]/[150,150,185,255]`,测试输出 `... ok`。
4. **端到端验证 —— 未执行**。按用户指示已停止端到端测试(headless 服务 `POST /pages` → `/pipelines` → `/export` 流程未跑),不得记为通过。
5. **等价性 —— 通过(容差内)**。`golden_equivalence` 4 passed:G1 `diff vs golden: max=0, pixels_over_1=0`;G2 同样 `max=0, pixels_over_1=0`(严于每通道 ≤1);G3 `outside rect: max=0 … over_1=0; inside rect: max=6 at (35,32), mean=4.863, pixels>32=0/4096; overall: max=6, mean=1.216`(矩形外背景逐像素零差);G4 `flat_violations=0 (max 0 at (0,0)); glyph max=1 at (21,6), violations>8=0; near-black pixels golden=341 current=341; non-background pixels golden=763 current=758`。既有 `rendering.rs::rasterized_png_contains_source_and_cleanup_pixels` 断言随 rendering 5 passed 保持。基线 PNG 采集自修复前未修改实现(vello 0.9.0,`test result: ok. 4 passed; 0 failed; 0 ignored`),抽样像素:G1 (0,0)=[125,25,110,255]、(10,10)=[200,100,50,255];G2 (0,0)=[68,23,155,255]、(10,10)=[105,60,125,255]。
6. **文档 —— 本节即为记录**。修复后已在本文件末尾追加「修复记录」,结论行给出 commit hash 与选项 A。

### 已知差异

- **G3 缩放重采样滤波差异**:缩放/非整数定位的图像层从 vello 的双三次(Mitchell)滤波改为 CPU 最近邻取样(`compose.rs:122-151`),故 G3 矩形内 mean=4.863、max=6(阈值 mean≤20/max≤64/单点≤16 内,`pixels>32=0/4096`),无结构丢失;样本 (64,64) current=[128,128,128,255] vs golden=[122,122,128,255] 即此滤波差。1:1 整数放置走精确拷贝路径不受影响。放大呈块状、缩小可能混叠;如需与 vello 完全一致需实现卷积滤波,超出本次最小改动范围。
- **G4 字形像素计数微差**:非背景像素 current=758 vs golden=763(抗锯齿边缘 5 像素差,结构容差 ±20% 内);严格逐点 `flat=0 / glyph max=1`。文字光栅依赖本机字体发现(fontique fallback,本机含 CJK 字体),跨机器复现字形度量可能不同,等价性比对须在同一台机器进行。

### 遗留事项(gaps)

- 端到端验收(第 6 节第 4 条)未执行(按用户指示停止)。
- 本次未验证「普通 `cargo test` 不带 `--ignored` 时不会覆盖基线」的实际行为(`#[ignore]` 是 Rust 标准语义,额外跑一次会消耗测试预算,故未执行)。
- `golden_capture.rs` 采集测试无像素断言且每次运行无条件覆盖 `tests/golden/*.png`,唯一保护是 `#[ignore]` 与注释;一次带 `-- --ignored` 的常规全量测试会静默冲掉修复前唯一基线,建议后续加备份或断言保护。
- 图文交错时每个 CPU 图像层触发一次全尺寸 GPU 渲染+读回+CPU 混合,交错 k 处即 k 次额外全尺寸拷贝与阻塞式同步(典型层序只有 1 个 run,标低危;见 review `raster.rs:281`)。
- 分层路径在 GPU 尺寸校验前分配 factor² 倍全尺寸 CPU 画布(`raster.rs:246`),超大帧有 OOM 风险(见 review)。
- `crates/koharu-renderer/src/compose.rs:8` 仍残留 `#![allow(dead_code)]` 临时豁免(当时为等待光栅化编排接入;接入已完成,该豁免可移除但本次未动)。review 所列次级隐患(重采样滤波退化、图文交错多次全尺寸读回、画布先于设备上限校验分配)均未在本次最小改动中处理。
- 复现全量测试需在任务书 pkg-config shim 之上追加 `LIBCLANG_PATH`、`BINDGEN_EXTRA_CLANG_ARGS` 与 `/tmp/fakebin/cmake wrapper` 三个环境准备(已写入各阶段 evidence 供后续沿用)。
- G4 文字基线与本机字体绑定,跨机器比对需另行论证。
