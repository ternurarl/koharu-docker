# PDF 与归档导入移植任务计划

日期：2026-10-16。状态：待实施。上游来源是 `koharu-rs/koharu` 的 `79cc4d6c`（`feat: support archive and PDF page imports`），落点是 fork 的 `crates/koharu-rpc`。设计细节取自 `plan/upstream-sync-audit-fixes.md` 第 4 节，本文件是实施任务书。

## 目标与范围

`POST /projects/current/pages` 的 multipart 上传从只接受 PNG/JPEG/WebP 单图，扩为接受单图加 zip/cbz、rar、pdf。路径、方法、响应类型不变。上游实现全部在 `crates/koharu-app/src/commands/import/` 的 GUI 命令层，按 cherry-pick 无法落到 headless，需要在 `crates/koharu-rpc` 重写一层。

排除项：`crates/koharu-app/**`、`crates/koharu-desktop/**`、`packages/**` 不动；`crates/koharu-app/Cargo.toml` 的依赖改动不取。

## 上游实现的构成

上游 `79cc4d6c` 新增四个文件，都是 `pub(super)` 的 GUI 命令层私有代码，不经任何公共 crate。

| 上游文件 | 行数 | 内容 |
| --- | --- | --- |
| `crates/koharu-app/src/commands/import/mod.rs` | 148 | `Format` 枚举（Raster / Zip / Rar / Pdf，用 `strum` 从扩展名解析）、`EncodedPage`（名字加原始字节）、`Page`（名字加字节加 `image::ImageFormat` 加宽高）、`decode`（从字节猜格式并读尺寸）、入口 `import(Vec<PathBuf>)`（按文件名自然排序、`rayon` 并行、按扩展名分派） |
| `crates/koharu-app/src/commands/import/pdf.rs` | 133 | `render(&Path)` 用 `hayro` 把 PDF 逐页渲染成 300 DPI PNG（`PDF_SCALE = 300.0 / 72.0`，白底），页名 `{stem}-{页码:0宽度}.png` |
| `crates/koharu-app/src/commands/import/zip.rs` | 140 | `extract(&Path)` 用 `zip::ZipArchive` 读 ZIP/CBZ，只留光栅图扩展名成员，跳过 `._*`、`.DS_Store`、`Thumbs.db`、`desktop.ini`、`__MACOSX`，按成员名自然排序 |
| `crates/koharu-app/src/commands/import/rar.rs` | 152 | `extract(&Path)` 用 `rars::ArchiveReader::read_path` 加 `extract_to` 回调，过滤规则同 zip.rs，`PageWriter` 在 `Drop` 时把收集到的成员字节推入结果 |

上游 GUI 侧接线在 `crates/koharu-app/src/commands/lifecycle.rs` 的 `import_pages`：文件对话框过滤器改为 `import::Format` 的全部扩展名，解码逻辑改为调用 `import::import(files)`。这一层不移植。

## 新建文件与职责

在 `crates/koharu-rpc/src/import/` 下新建四个文件，模块挂进 `crates/koharu-rpc/src/lib.rs`（现有 `mod routes;` 旁加 `mod import;`）。

| 新文件 | 从上游搬什么 | 要重写什么 |
| --- | --- | --- |
| `crates/koharu-rpc/src/import/mod.rs` | `Format` 枚举、`EncodedPage`、`Page`、`decode` 全部照搬；`import` 的排序、并行、分派骨架照搬 | 入口签名从 `import(Vec<PathBuf>)` 改为 `import_uploads(Vec<(String, Vec<u8>)>) -> anyhow::Result<Vec<Page>>`，输入是 multipart 上传的（文件名，字节），不读文件系统；格式识别从「路径扩展名」改为「上传文件名扩展名加 `image::guess_format` 兜底」；删掉 `fs::read` 一段 |
| `crates/koharu-rpc/src/import/pdf.rs` | `PDF_SCALE` 常量、页命名规则、`hayro` 的 `InterpreterSettings`/`RenderSettings`/`RenderCache` 用法、单页测试夹具 | `render(&Path)` 改为 `render(name: &str, data: &[u8])`，`Pdf::new` 直接吃字节；错误消息里的 `path.display()` 换成上传文件名 |
| `crates/koharu-rpc/src/import/zip.rs` | 成员过滤规则、自然排序、空容器报错 | `File::open(path)` 加 `ZipArchive::new(file)` 改为 `ZipArchive::new(std::io::Cursor::new(data))`；测试夹具改为内存构造 |
| `crates/koharu-rpc/src/import/rar.rs` | `PageWriter`（`Write` 加 `Drop` 收集）、过滤规则、自然排序 | `ArchiveReader::read_path(path)` 需要内存来源。`rars` 0.6 是否提供接受 `Read` 或字节的构造入口，实现时查 `rars` 0.6.0 文档确认；若只有路径入口，把上传字节写进 `std::env::temp_dir()` 下的唯一临时文件再 `read_path`，用完删除 |

`crates/koharu-rpc/src/project.rs:177-206` 的 `import_images` 签名与实现不改，新模块的 `Page` 输出转换成它已有的五元组即可。

## 与 headless 命令面的对齐

`POST /projects/current/pages` 的路径、方法、响应类型（`Json<ProjectInfo>`）全部不变，变化只在 multipart 字段可接受的内容类型。一个上传字段展开成多页时，页名沿用上游规则：压缩包成员名，PDF 为 `{stem}-{页码:0宽度}.png`。`HEADLESS.md:93` 的描述改为「multipart page import (images, zip/cbz, rar, pdf)」，`HEADLESS.md:140` 不动。不需要新路由与查询参数，格式由上传文件名扩展名识别，序列化值与上游 `Format` 枚举一致（`png`/`jpg`/`jpeg`/`webp`/`cbz`/`zip`/`rar`/`pdf`）。

`crates/koharu-rpc/src/api.rs:22` 附近的 `DefaultBodyLimit` 已是 1 GiB。PDF 逐页渲染的峰值内存是一整页 RGBA（A4 300 DPI 约 33 MiB）加 PNG 编码缓冲，单请求处理放在 `spawn_blocking` 里，不占 async worker。

## 依赖与改动行数

工作区 `Cargo.toml` 的外部依赖段（第 44 行起）新增 `hayro = "0.7.1"` 与 `rars = "0.6.0"` 两行，`crates/koharu-rpc/Cargo.toml` 引入这两个依赖。`zip`（工作区 `Cargo.toml:80`）、`image`、`webp`、`rayon`、`alphanumeric-sort`、`strum` 工作区已有，不新增。`Cargo.lock` 由 cargo 重新生成，禁止手改。

预估改动行数合计 640 至 680 行，其中约 360 行是从上游搬来或小幅改写的生产代码，约 180 行是搬来的测试：

- `crates/koharu-rpc/src/import/mod.rs` 约 170 行
- `crates/koharu-rpc/src/import/pdf.rs` 约 150 行
- `crates/koharu-rpc/src/import/zip.rs` 约 130 行
- `crates/koharu-rpc/src/import/rar.rs` 约 170 行
- `crates/koharu-rpc/src/routes/projects.rs` 的 `import_pages`（现有 129-173 行）替换其中约 30 行解码代码为约 20 行调用
- `crates/koharu-rpc/src/lib.rs` 加 1 行，两个 `Cargo.toml` 各加 2 行

## 验证与测试

统一验证命令，全部完成后跑一次：

```text
cargo check -p koharu --no-default-features --features headless
```

分项验证命令，每完成一个文件跑一次：

```text
cargo check -p koharu-rpc
cargo test -p koharu-rpc --lib
```

最小测试覆盖：

1. 上游四个文件的测试照搬：自然排序、PDF 300 DPI 尺寸断言、zip 成员过滤、rar 抽取。
2. REST 层补一条 `import_pages` 的 multipart 测试：上传一个两页 PDF 的字节，检查并要求返回的 `ProjectInfo.pages` 长度等于 2。
3. rar 的临时文件回退分支若被采用，补一条「上传字节写入临时文件后能被 `read_path` 读出且临时文件被删除」的测试。

## 实施顺序

第一步 `import/mod.rs` 加 `import/zip.rs`，先让单图与 zip 路径通。第二步 `import/pdf.rs`，PDF 是本次主要收益。第三步 `import/rar.rs`，先确认 `rars` 0.6 的构造入口形态再决定是否走临时文件回退。第四步 `routes/projects.rs` 接线与 `HEADLESS.md` 描述更新。第五步统一验证与 multipart 测试。

每个文件一个独立提交，正文写明搬自上游 `79cc4d6c` 的哪些函数、重写点在哪，结尾附 `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`。

## 验收标准

`POST /projects/current/pages` 接受单图、zip/cbz、rar、pdf 四类上传并正确展开页数；PDF 页尺寸符合 300 DPI；压缩包成员过滤规则与上游一致；`crates/koharu-app`、`crates/koharu-desktop`、`packages/**` 无改动；`cargo check -p koharu --no-default-features --features headless` 通过。
