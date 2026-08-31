# Rust 原生 Markdown 组件调研（2026-08-31）

> 实施决定（2026-08-31）：在两个原型完成真人体验后，项目已按使用者决定迁移到
> `gpui-component 0.5.1`。本文前半部分保留当时的调研判断，便于追溯；最终选择以
> 实际滚动体验、较低的常驻内存，以及组件原生支持 Markdown + 简单 HTML 为依据。

## 结论

目前没有一个现成 Rust 组件能同时满足以下全部要求：

- Markdown 与受限 HTML 混排；
- 长文档只渲染可见区域；
- 图片按显示尺寸解码，并受软/硬内存预算控制；
- 保留源文件字节，不引入浏览器/WebView；
- 在 Windows 上保持极低 CPU 与内存占用。

但已经不需要自行实现完整 Markdown 渲染器。优先方案是复用成熟解析/排版组件，只保留两个很薄的应用层模块：HTML Lite 适配器和受预算约束的图片加载器。

## 候选方案

| 方案 | 优点 | 关键缺口 | 建议 |
|---|---|---|---|
| `egui_commonmark 0.25` | 与现有项目迁移距离最短；新增 `render_html_fn`；Markdown、表格、图片和语法高亮成熟 | HTML 回调主要覆盖块级 HTML；没有稳定公开的文档级虚拟化接口；没有按字节图片预算 | 低风险基线，必须进入实测 |
| `egui_commonmark_extended 0.25` | 公开 `show_scrollable`，按可见视口裁剪；支持 `content_version` 避免每帧全文哈希；保留 HTML 回调 | 第三方项目维护的 fork，绑定较旧 egui；行内 HTML 与图片预算仍需自理 | 当前最接近性能目标的候选，优先做 spike |
| `gpui-component 0.5.1` `TextView` | 原生支持 Markdown + 简单 HTML；`scrollable(true)` 使用虚拟列表 | 不支持所需的全部 CSS/标签；图片安全与内存策略不完整；官方明确复杂 Markdown viewer 可能需要 fork；整套 UI 迁移成本高 | 作为跨框架性能对照，不直接迁移 |
| Qt `QTextDocument` | Markdown、HTML、富文本和资源接口完整，系统级排版成熟 | Rust/C++/Qt 构建与部署复杂；Markdown 嵌在 HTML block 内有语义限制；全文布局和内部缓存不利于严格内存控制 | 只有在兼容性优先于轻量化时考虑 |
| Iced Markdown | 官方组件，支持增量解析、图片和可替换的已知块渲染 | 数据模型没有 raw HTML item；没有现成文档级虚拟化和缓存预算 | 不值得为本需求迁移 |
| `egui_markdown 0.1` | 新实现，会跳过不可见块/图片 | raw HTML 当普通文本；版本太年轻 | 暂不采用 |
| Slint / Floem | 原生、通用虚拟列表能力不错 | Slint Markdown 范围有限；Floem 没有官方 Markdown 组件 | 仍等同于自研渲染器底座 |

## 最值得验证的两条路线

### A. 保留 egui，采用 `egui_commonmark_extended`

这条路线最符合“性能第一但不做无谓重写”。`show_scrollable` 的公开文档明确说明：首次绘制建立 waypoint，此后只渲染与可见视口相交的事件；`content_version` 可以避免每帧对全文计算哈希。它也保留了 `render_html_fn`，足以接住样本中块级的 `<div style="text-align: center"><img ...></div>`。

风险是它属于应用方维护的 fork，而非上游主线；还需要核实快速滚动、字体/窗口宽度变化和图片异步载入导致高度变化时的稳定性。

### B. 保留上游 `egui_commonmark`，应用层做章节虚拟化

上游 0.25 已提供 HTML block 回调，因此无需继续把 HTML 转成代码文本，也无需重写 Markdown 解析和普通块渲染。应用按标题切为稳定 section，配合可变高度虚拟列表，可以获得更可控的生命周期和较低的依赖风险。

代价是应用仍需维护 section 高度缓存。上游源码中的内部可滚动接口并不是一个适合依赖的稳定公开 API。

## 为什么 GPUI 不是直接答案

`gpui-component::TextView` 是跨框架候选中最接近需求的：它会解析 Markdown 和简单 HTML，并通过 `gpui::list` 虚拟化长内容。源码还能处理图片 `width`/`height` 的 px 与 `%`。

但现有实现没有覆盖本项目约定的 `text-align`、`sub/sup` 等全部语义，也没有证据表明其图片资源缓存支持 48 MiB 软预算、160 MiB 硬上限、停止滚动后延迟淘汰及按目标尺寸解码。换框架后这些模块仍要自行实现，因此只有测得显著性能优势才值得迁移。

## 图片仍必须由应用接管

egui 0.36 的 loader API 提供 `byte_size`、`forget`、`forget_all` 和 `Context::forget_image`，并支持 `SizeHint`。这给实现应用级 LRU 和主动释放提供了接口，但默认 loader 并不实现本项目约定的软/硬预算策略。`reduce_texture_memory` 可以在纹理上传后丢弃 CPU 侧图像数据，减少一份副本，但不能代替完整预算管理。

因此建议将图片加载独立为一个小而深的模块：

1. 只接受文档目录内的本地相对路径；
2. 读取尺寸头后先执行文件大小、像素数和尺寸上限检查；
3. 后台解码到 `viewport × DPI` 所需尺寸；
4. CPU 图像与 GPU 纹理统一计账；
5. 48 MiB 为软预算，滚动时可临时突破并在状态栏提示；160 MiB 为硬上限；
6. 停止滚动两秒后按 LRU 淘汰，系统内存压力可提前触发；
7. 不写入磁盘缓存。

## 建议的实测决策门

用同一份真实书稿同时制作两个隔离 spike：

1. `egui_commonmark_extended 0.25`；
2. `gpui-component::TextView 0.5.1`。

上游 `egui_commonmark 0.25 + section virtualization` 作为最终可维护方案的基线。三者统一记录：冷启动到首屏、稳定 RSS、连续快速滚动时峰值 RSS、帧时间 p95/p99、停止后的回落时间和空闲 CPU。任何无法保证 160 MiB 硬上限或无法显式释放图片资源的方案直接淘汰。

在没有基准数据前，不建议更换 GUI 框架。基于 API 能力和迁移风险，当前优先级为：

1. `egui_commonmark_extended` 性能 spike；
2. 上游 `egui_commonmark + section virtualization`；
3. GPUI Component 对照 spike；
4. Qt 仅作兼容性后备。

## 一手资料

- [`egui_commonmark 0.25` CommonMarkViewer](https://docs.rs/egui_commonmark/latest/egui_commonmark/struct.CommonMarkViewer.html)
- [`egui_commonmark 0.25` Cargo 配置](https://docs.rs/crate/egui_commonmark/latest/source/Cargo.toml)
- [`egui_commonmark_extended 0.25` CommonMarkViewer](https://docs.rs/egui_commonmark_extended/latest/egui_commonmark_extended/struct.CommonMarkViewer.html)
- [`egui_commonmark_extended` 来源项目](https://github.com/aydiler/md-viewer)
- [`gpui-component` Text 模块](https://docs.rs/gpui-component/latest/gpui_component/text/)
- [`gpui-component` HTML 解析源码](https://github.com/longbridge/gpui-component/blob/main/crates/ui/src/text/format/html.rs)
- [Iced Markdown 模块](https://docs.iced.rs/iced_widget/markdown/index.html)
- [`egui_markdown` 文档](https://docs.rs/egui_markdown/latest/egui_markdown/)
- [Qt `QTextDocument`](https://doc.qt.io/qt-6/qtextdocument.html)
- [egui `Context` 图片加载与释放 API](https://docs.rs/egui/latest/egui/struct.Context.html)
- [egui `ImageLoader`](https://docs.rs/egui/latest/egui/load/trait.ImageLoader.html)
- [Slint Markdown/富文本范围](https://github.com/slint-ui/slint/issues/9560)
- [Floem](https://github.com/lapce/floem)

## 原型实测更新（2026-08-31）

两个隔离的 release 原型已经用示例章节运行，原始记录与复现命令见
[`spikes/RESULTS.md`](../spikes/RESULTS.md)。

- `egui_commonmark_extended 0.25` 首次渲染约 120–129 ms，但静置和自动滚动
  的 RSS 都达到约 425–427 MiB。它的 viewport clipping 没有约束默认图片
  loader 的解码与纹理内存，因此不能原样进入产品。
- 第一轮 `gpui-component 0.5.1` 的约 108 MiB 数据没有加载本地图片，不能用于
  对比。补上文档目录内的相对图片加载适配后，首屏实际加载 2 张图片，3 秒后
  RSS 约 125 MiB；release 二进制约 16.09 MiB。
- egui 的 HTML 图片过小是布局约束错误：图片同时受到了滚动区“剩余高度”的
  限制。改为先采用原图尺寸、再限制最大宽度后，宽度回归测试通过，但 RSS
  仍约 438 MiB，说明显示尺寸修复并没有解决默认图片缓存问题。
- 后续公平性修正为 egui 的正文和代码字体链都加载微软雅黑，并将两边的滚轮
  总位移统一为系统每格 3 行 × GPUI 每行 20pt = 60pt。此时 3 秒静置 RSS 为
  egui 约 480 MiB、GPUI 约 139 MiB（首屏 2 张图片）。两框架自身的滚动动画
  和帧调度保持不变，作为体验比较的一部分。
- GPUI 的内部虚拟列表句柄不公开，本次 Windows UI 自动检查通道也不可用，
  所以该数字尚未覆盖滚过全部图片的压力场景，不能据此决定迁移框架。

最终由使用者在两套原型上实际阅读、滚动后选择 GPUI。主程序现已使用
`gpui-component::TextView`，并通过应用自己的 HTTP client 适配文档目录内的
相对图片；路径穿越会被拒绝，远程图片默认关闭且不使用磁盘缓存。TextView 外层
使用文档级 LRU 图片缓存，按解码帧与 GPU 纹理估算内存：48 MiB 软预算、滚动中
临时突破并在状态栏提示、空闲两秒回收，160 MiB 硬上限。原型继续保留，只作为
历史对照和后续回归基准。
