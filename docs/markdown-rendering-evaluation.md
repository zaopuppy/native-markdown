# Markdown 阅读架构评估与实现

> 后续更新：原生外壳已升级官方 GPUI Component 0.10.0 / gpui-pre 0.3.12，移除本地 fork。下文 Phase A 及旧测量保留历史基线；最新结果见文末“移除 fork 后的官方版本验证”。


日期：2026-11-19。基线：`9ebb1e2`，工作区初始干净。先分析关键调用链，再运行独立 Winit/Wry PoC，最后接入主程序；用户体验后明确授权彻底退休旧实现，现已删除。

## 决策

Windows 唯一阅读实现为 **GPUI 原生外壳、原生源码编辑器 + 一个 Wry/WebView2 子窗口预览**。空欢迎页不创建 WebView，回到真正的空文档时释放控制器。旧 TextView/merman 原生阅读路径、renderer 环境变量和失败回退均已删除。WebView 初始化失败时显示明确错误，源码编辑及保存仍可使用；非 Windows 目前没有受支持的阅读后端。

保留文件树、目录、阅读/分栏/源码模式、搜索、缩放、对话框、拖放、布局设置、原子保存和恢复。没有 Electron、Tauri、React/Vue、Node 运行时或前端打包器，没有捆绑 Chromium，也不会在启动时自行安装 Runtime。

选择的收益是浏览器文字选区、内联 SVG、公式和排版；代价是 WebView2 多进程私有内存和 Windows 混合窗口合成约束。它不是“Rust 所以省内存”的结论。完整实测见后文。

## Phase A：改造前架构与证据（历史基线）

| 链路 | 关键位置与实际行为 |
| --- | --- |
| 窗口 | `src/main.rs`：GPUI 0.2.2 单窗口，GPUI Component Root；无 Winit/Tauri 事件循环 |
| 应用 | `src/app.rs`：工具栏、两侧可调栏、视图模式、快捷键、搜索、文档生命周期 |
| 原生预览 | `preview_panel` 使用 vendored GPUI Component 0.5.1 的虚拟化、可选中文字 TextView；fork 提供代码块替换、图片激活和标题跳转接口 |
| 分析与编辑 | `src/markdown.rs` 用 pulldown-cmark 0.12 分析目录、搜索和字数；InputState 是独立源码编辑器，未编辑时也会创建文档状态 |
| 文件 | `src/document.rs` 读取 UTF-8；保存写同目录临时文件、sync、persist；保存内容来自源码而非渲染 HTML，保留 dirty 检查和恢复 |
| Mermaid | `src/mermaid.rs`：固定 merman 0.7.0，独立持久 worker、超时、来源校验和按源缓存；不执行 Mermaid.js |
| SVG | `src/image_loader.rs::rasterize_mermaid_svg`：resvg/usvg 转 PNG，再进入 GPUI 图片路径；预览宽度上限 1280，像素预算 4,194,304；查看器另有高分辨率预算 |
| 图片与滚动 | `src/image_loader.rs` 限定文档目录、拒绝越界；`src/image_cache.rs` 当前软预算实际是 100 MiB；TextView 虚拟滚动、图片查看器、缩放档位均已存在 |
| 设置与主题 | `src/layout_settings.rs` 保存侧栏状态与宽度；现有窗口配色多为固定浅色，没有验证到用户主题切换按钮；不虚构原有主题能力 |
| 浏览器依赖 | 原 Cargo 主依赖没有启用 WebView。vendored UI 有未启用的 `webview` feature（可选 lb-wry），不能据此认为原程序已运行浏览器 |

确认的问题是 **SVG 被光栅化后不再有可选标签或 DOM 交互**；提高 PNG 尺寸不能恢复这些能力，也不能保证无限放大清晰。原生程序已有图片缩放平移和正文选区，因此不把这些功能误报为完全缺失。

资源方面，已有代码缓存阅读统计、限制字号档位、清理文档图片，并非每帧都解析全文。此次 602,110 字节、1600 个章节的合成文档确实提高了原生私有提交，但没有证据把它归结为泄漏。原源码编辑器与分析状态仍会保留大文档成本，不能把最终全部内存归因于 WebView。

依赖维护核查（crates.io/npm 元数据，2026-11-19）：GPUI 当前发布仍为 0.2.2；GPUI Component 上游已到 0.10.0，而本地是 0.5.1 fork；merman 当前 0.9.4、resvg 0.50.0、pulldown-cmark 0.13.6，均比项目锁定版本新。初次混合实现阶段曾保留这些原生依赖；最终清理已移除应用的原生渲染专用依赖，没有混入升级。新 Wry 固定为 0.57.3；Windows 使用系统 WebView2，其他平台的 WebKit/WKWebView 集成此次未实现或实测。

## Phase B：候选比较与独立 PoC

| 方案 | 体验与复用 | 成本、工程与平台判断 |
| --- | --- | --- |
| 继续原生 | 保留全部代码、基础内存最低；现有 Mermaid 是有界 PNG | 做清晰选区、SVG DOM、复杂排版和公式需要新增大量自有实现，不能通过小改达到目标 |
| GPUI + 一个 Wry 子窗口 | 浏览器承担整个预览区域；编辑、文件和周边 UI 全部复用 | 最小侵入；需处理 Windows 合成、焦点和 native child 的遮挡，已实际验证并处理 |
| Winit + Wry 整窗 | 独立 PoC 已运行，浏览器体验相同；能省掉 GPUI 的外壳成本 | 必须重做文件树、设置、快捷键、目录、搜索与编辑 UI；未用简陋 PoC 冒充完整产品的内存对照 |
| Tauri 2 | 同样使用 WebView2，可以不依赖前端框架，具有权限及打包体系 | 没有理由认为同一个浏览器引擎会天然更省内存；本任务不需要其新增应用层，未构造或宣称 Tauri 实测数据 |

独立 PoC 位于 `spikes/webview-reader`（Winit 0.30、Wry 0.57.3），与主程序共享 HTML 投影及离线资源。早期 PoC 在大文档上约 585 MiB 私有提交；改成资源协议传文档、可见区高亮后仍约 523 MiB。按顶层章节使用 `content-visibility:auto` 后，3 次同文档实测为 **196.2 MiB 中位数，194.0–197.9 MiB**，采样峰值中位数 200.4 MiB。该组是 Mermaid 11.15 / 早期资源的 PoC 数据，只用于确认离屏排版优化有效，不代替最终应用数据。

PoC 的真实 WebView2 自动化已验证内联 SVG（不是 `<img>`）、中文 SVG 标签、文字选区、Ctrl+C 和系统剪贴板、按钮缩放、指针拖拽平移、Markdown 正文共存、坏图隔离、图片、公式、高亮与深色媒体查询。标签验证曾失败：仅设置 flowchart 的 htmlLabels 不够，必须同时关闭全局 htmlLabels，才能在去除 foreignObject 后保留真正的 SVG 文字。

## Phase C：实际实现与安全边界

- `src/web_document.rs`：复用 pulldown-cmark，新增数学事件、章节容器、ammonia 清洗、安全 HTML 表格跨度；只给经过 canonicalize 的目录内图片和 Markdown 链接发放代际 token。文档切换使旧 token 失效；协议只服务固定内置资源和已登记图片，不把 URL 当文件路径。图片限制为单个 32 MiB 源文件，读取也有长度上限。
- `src/web_preview.rs`：一个懒创建、重复使用的 Wry 子窗口，布局来自 GPUI canvas 的实际 bounds；校验 IPC 来源和消息长度。构造由异步任务调度，避开 render 和 NativeMarkdownApp 的可变借用；加载期间保持固定阅读区域。只接受固定快捷键、滚轮、已登记链接等消息，不存在任意文件读写、脚本求值或通用 invoke 命令。OS 拖放经单独 Rust 事件通道接入，网页不能伪造带路径的拖放事件。状态栏图片数读取预览登记的本地图片引用，不再误用原生图片加载计数。
- `assets/web/reader.js` / `reader.css`：无框架，Mermaid 内联 SVG；Fit、5%–800% 图表缩放、Ctrl+滚轮、Pan 模式拖拽；正常模式保留选区；图片模态查看器；DOM 搜索与目录导航；公式和代码高亮按需加载，代码与图表按接近视口时处理。
- Windows 必须在 GPUI 初始化前设置 `GPUI_DISABLE_DIRECT_COMPOSITION=true`，否则 DOM 已渲染但主窗口看起来是空白。实际窗口已复现并验证修复。原生菜单、窄窗口浮层和对话框出现时暂时隐藏 WebView，关闭后恢复，避免子 HWND 遮挡菜单。首次 WebView ready 后重试文件树扫描；原生查找快捷键显式转移系统焦点。
- 离线库固定为 Mermaid **12.3.0**、KaTeX **0.19.1**、highlight.js **11.12.0**、DOMPurify **3.4.18**。`scripts/fetch-web-assets.py` 从 npm 拉取指定版本、校验包完整性与既有锁值，只复制允许的文件；许可证和逐文件 SHA256 在 `assets/web/vendor/manifest.json` 旁。普通构建不下载这些资源。

不可信 HTML 经 ammonia 清洗；后续文档替换再经 DOMPurify。SVG 使用 strict、安全 SVG 配置及再次清洗，去除事件、外链、image、a、foreignObject，且不绑定图表事件。禁用文档 Mermaid 配置、click 功能和不可信 HTML 标签；最多 64 图、每图 256 KiB、1500 条边。KaTeX trust=false。CSP 禁止外部网络、frame、object、base 和表单提交，资源带 nosniff；禁止新窗口、下载和权限请求。调试端口仅在显式设置测试环境变量时开启，日常运行不开端口。WebView2 本身仍需由系统及时更新。

## 最终清理（用户确认统一技术栈后）

- 删除 `src/mermaid.rs`、`image_loader.rs`、`image_cache.rs`、`image_viewer.rs`，包括 worker 协议、原生 SVG 光栅化、100 MiB 图片缓存和原生模态查看器；图片/图表查看统一由浏览器 DOM 承担。
- 删除 vendored TextView、Markdown/HTML formatter、文档节点/样式和文档选区全局状态；保留源码编辑器搜索标记和有背景高亮的通用文本。语言服务文档提示以普通文本显示，不再隐含另一套 Markdown 阅读器。
- 移除应用直接依赖 merman、resvg、image、sha2、gpui_http_client 和 worker 专用 Windows feature；移除组件依赖 markdown、html5ever 0.27、markup5ever_rcdom、旧 lb-wry。相对原生基线，92 个旧锁定包不再出现在 Cargo.lock。resvg/image/gpui_http_client 仍是 GPUI 底层图形运行时的间接依赖，无法在保留 GPUI 外壳时直接删除。
- 删除原生 renderer 环境变量、失败回退、worker 集成测试、旧图片缓存/原生阅读测试、原生 egui/GPUI PoC 与旧预算脚本；保留历史测量 JSON，标注旧调研为历史记录。当前测量入口只针对 WebView；旧基线 exe 已清理。
- 清理本项目 target 及三个 PoC 的 target，共删除约 22 GiB 构建缓存；全局 Cargo registry、其他项目、布局设置及未保存文档恢复数据没有作为缓存删除。随后仅重建当前依赖图。
- 退休原生预算脚本后，内部测量默认也不再套用旧 160 MiB 门槛；仅当显式配置预算时才执行门槛检查。
- 打开文档时统一保存绝对路径，使命令行相对文件名也有正确的目录和本地资源根；Windows 保留普通绝对路径形式，避免 extended-path 前缀破坏文件树路径比较。

清理后当前 53 项单元/GPUI 测试、clippy（拒绝警告）、fmt 和真实 WebView 功能验收通过，空窗口延迟打开连续 3 轮通过。移除了已无实现对象的原生 renderer / worker / 图片缓存测试，阅读能力由真实 WebView 验收接替，没有用 headless 占位界面冒充浏览器通过。故意把 WebView profile 指向普通文件，实际验证错误反馈、切换源码、输入和 Ctrl+S 磁盘保存；没有恢复原生阅读 fallback。

复测中修复了测试工具的固定调试端口与前次浏览器退出之间的竞争，以及临时 profile 文件句柄延迟释放造成的清理错误。并行编译和 GUI 测试时另出现过一次 5 秒场景结束而 WebView 未就绪、25 秒退出超时，尚未完全归因；停止并行负载后的连续 3 轮通过，不能据此宣称已证明极端负载下的退出稳定性。

可执行文件由混合版本 45,541,376 B 缩至 27,191,296 B（约 43.4 → 25.9 MiB）；这是磁盘体积，不当作运行内存收益。

下方表格保留首次混合实现阶段的历史比较，不能把其中“最终”字样当作每个后续构建已经复测。新的交付构建与验证记录单独记录。

## 统一 WebView 后的交付版本实测

以下为删除旧实现后的 **当前 release**（SHA256 `b2fb863297947236c5ddc3ccfa3841598e3ba0dd6abe0acc84a365dd7c764a01`），同一机器/Runtime、相同生成文档、每场景 3 个新进程、每次 12 秒。构建和测试均已结束，没有同时运行编译；后台桌面仍有其他应用。沿用已存在的独立测量 profile，属于暖缓存测试，未测真正冷启动或 Runtime 首次安装。

所有关联子进程计入。Private Bytes 是私有提交，Private WS 是驻留私有页；WS 的共享页可能重复计数，不把 WS 合计称为独占物理内存。逐进程 PID、职责、三个内存指标、采样峰值、CPU 和时间事件见 [当前构建测量证据](measurements/webview-only-2026-11-19.json)。15 次运行 stderr 为空，没有 benchmark error；清空后的末次采样只剩宿主。

| 场景 | Private Bytes 中位数 [最小–最大] MiB | 采样峰值中位数 MiB | Private WS 中位数 MiB |
| --- | ---: | ---: | ---: |
| 无文档 | 65.8 [65.8–65.9] | 65.9 | 48.8 |
| 普通 Markdown | 228.8 [227.0–234.2] | 233.0 | 153.8 |
| 8 个 Mermaid 图 | 305.7 [305.3–306.3] | 314.6 | 227.6 |
| 大 Markdown，602,110 B | 392.0 [391.9–392.9] | 395.1 | 298.6 |
| 交替打开 5 次后清空 | 70.3 [70.2–70.3] | 363.4 | 52.6 |

普通文档第 3 轮末次进程快照（MiB；不是各进程中位数）：

| 职责 | Private Bytes | Private WS | WS（含共享） |
| --- | ---: | ---: | ---: |
| Rust/GPUI 宿主 | 69.3 | 51.5 | 98.9 |
| WebView2 browser | 40.5 | 32.2 | 127.9 |
| crashpad | 2.8 | 1.8 | 10.7 |
| GPU | 72.3 | 44.7 | 94.9 |
| network | 11.5 | 6.5 | 38.0 |
| storage | 7.3 | 2.7 | 19.1 |
| renderer | 30.5 | 19.6 | 68.2 |

main 到 DOM 可读代理时间：普通 791 [658–817] ms、多图 740 [734–851] ms、大文档 977 [976–1087] ms。首次可见 Mermaid 本身 208.7 [199.2–256.8] ms，进程启动后 943 [935–1098] ms 完成；多图首屏附近仍是 4/8 懒渲染，不能冒充全部图完成。Rust 切换 handler 中位数 1 ms，范围 0–3 ms，不包含浏览器后续排版。此批普通文档末段 CPU 为 6.42% [4.64–9.58] 单逻辑核，16 核总体约 0.40%；包含内部场景驱动，不能与旧 Passive 批次直接等同。

相对历史原生基线，普通/多图/大文档分别多约 157/226/146 MiB 私有提交。相对初次混合实现，**没有证明删除旧代码让运行内存明显下降**：其主要收益是去掉第二套渲染与缓存维护、缩小二进制和依赖图；浏览器引擎成本仍存在。本次普通/多图/大文档接近此前约 232/306/392 MiB 批次，但高于最后较低的 227/295/371 MiB 批次；不能未经严格控制就把差额归因为代码收益或退化。

## 初次混合实现的历史测量方法与结果

Windows 11 内核 build 26300，系统产品标签为 Windows Home China；Intel Core Ultra X7 358H，16 个逻辑 CPU，约 32 GiB RAM。Release 构建，1180×780 逻辑窗口，使用相同生成文档、相同本机环境和用户布局设置。原生基线先独立保存 exe，再仅加入 `switch-idle` 测量入口保存 instrumented baseline；未混入 WebView 代码。每个最终场景 3 个新主进程、每次约 12 秒；没有同时编译或故意运行负载。

`scripts/measure-rendering.ps1` 从根 PID 递归追踪后代，记录每个进程的 PID、父 PID、类型、命令行、路径、CPU 时间、Working Set、Private Working Set 和 Private Bytes。包括 merman worker / WebView2 browser、renderer、GPU、网络及辅助进程。Private Bytes 表示私有提交，Private WS 表示驻留私有页；**WS 合计包含共享页的重复计数，不是独占物理内存**。不试图把共享页再次摊销为“真实占用”；GPU 专用/共享显存未单独计量。峰值是采样峰值，不是亚毫秒真实峰值；CIM 盘点使实际采样间隔长于 250 ms。

Runtime 已安装，实际进程路径确认版本为 **157.0.4341.60**；测试期间 WebView2 使用独立于其他应用的数据目录。测试使用已经建立的数据目录与系统文件缓存；没有清系统页缓存、重启或卸载 Runtime，因此**没有严格冷启动或首次安装成本数据**。首个窗口句柄时间受采样粒度影响，不等于首屏可读。最终 WebView 的 `document-readable` 是从 Rust main 到浏览器双 requestAnimationFrame 的代理时间，包含宿主启动，不包含全部离屏图表完成；原生没有对等埋点，不作首屏速度优劣结论。

下表均为 MiB，末次采样私有提交的 **3 次中位数 [最小–最大]**；峰值列同样取 3 次峰值的中位数。**版本边界：这是已经落地的 WebView 渲染版本的主测量批次，早于最后的状态栏图片计数和异步初始化修正。最后修正后的功能验收已完成，但完整性能矩阵尚未重跑，不能把下表或启动时间当成最终二进制的已复测保证。** 本轮用户随后按 Esc 停止桌面操作，未继续启动 GUI 基准。

| 场景与共同输入 | 原生基线 Private Bytes | 初次混合实现 Private Bytes | 初次混合实现峰值 | 原生 / 初次混合实现 Private WS 中位数 |
| --- | ---: | ---: | ---: | ---: |
| 空欢迎页，无文档 | 66.6 [66.0–71.8] | 65.4 [65.3–65.4] | 65.6 | 49.3 / 48.4 |
| 普通 Markdown，4,513 B | 71.7 [71.1–72.1] | 226.5 [225.6–228.0] | 227.7 | 53.7 / 151.8 |
| 8 个 Mermaid 图，7,245 B | 79.4 [79.2–79.8] | 295.0 [294.5–295.5] | 302.3 | 61.0 / 218.4 |
| 大 Markdown，602,110 B，1600 节 | 246.4 [244.4–247.0] | 370.9 [369.0–371.6] | 372.9 | 208.8 / 280.5 |
| 普通/多图切换 5 次，再清空并空闲 | 84.3 [80.0–85.7] | 69.9 [69.7–70.0] | 351.5 | 64.6 / 52.3 |
| 额外压力：351 节点、350 条边，12,220 B | 83.4 [82.4–83.8] | 377.6 [376.0–379.4] | 391.2 | 62.0 / 301.5 |

原生多图基线在预热时 8/8 ready，额外大图 1/1 ready，无渲染错误。WebView 多图场景首屏附近完成 4/8 图，其余懒渲染；这是架构行为差异，不能将该数当成“全部图表执行完”的等价测试。大图的整个 SVG 已生成，三个测次均完成。

最终普通文档比原生多 **154.8 MiB 私有提交 / 98.1 MiB 私有驻留**；多图多 **215.6 / 157.4 MiB**，大文档多 **124.5 / 71.7 MiB**。按本任务阅读体验优先的排序，接受这笔成本来获得成熟选区、图表交互和排版；原生低资源模式在用户确认最终技术栈后已退休，历史结果只作为成本对照。切回空文档后浏览器后代全部退出，最终只剩宿主；未观察到每次切换保留一个浏览器组的现象。12 秒数据不能证明长期无泄漏。大文件是多标题合成压力文档，结果不应推广到所有 600 KiB 文件。

区间只表示最后一批 3 次运行的波动。此前相同渲染路径普通/多图/大文档约 232 / 306 / 392 MiB；后续任务列表标记改动不涉及这些输入，不能把批次间下降宣称为代码优化收益。原始批次保留供复核，实际预算宜预留余量。

普通文档第 3 轮末次采样明细如下；这是一个真实快照，不能拿逐项数值当作各进程的中位数。完整 PID、父子关系、命令行、每轮明细、峰值、CPU 与事件保存在 [测量证据 JSON](measurements/markdown-rendering-2026-11-19.json)。

| 进程职责 | Private Bytes | Private WS | WS（含共享） |
| --- | ---: | ---: | ---: |
| Rust/GPUI 宿主 | 68.3 | 50.7 | 96.1 |
| WebView2 browser | 38.3 | 29.1 | 122.3 |
| crashpad-handler | 2.8 | 1.8 | 10.7 |
| gpu-process | 71.2 | 43.8 | 90.8 |
| network utility | 11.4 | 6.4 | 37.9 |
| storage utility | 7.3 | 2.7 | 19.0 |
| renderer | 28.6 | 17.7 | 65.2 |

暖缓存进程启动到 DOM 可读代理时间：普通文档 **636 ms [581–1065]**，多图 **619 ms [580–771]**，大文档 **1035 ms [895–1049]**，大图正文 **639 ms [629–731]**。首次可见 Mermaid（含库加载、布局、清洗与 DOM 安装）**213.0 ms [194.0–216.3]**；该图在进程启动后 **805 ms [787–982]** 完成。351 节点大图首次生成 **885.8 ms [812.4–1131.5]**，进程启动后 **1517 ms [1409–1854]** 完成。后续可见图的渲染成本更低，不能混入首次加载中位数。

切换场景原生和最终的 Rust handler 都为 **1 ms 中位数**，范围分别 0–3 / 0–2 ms；这不包括后续排版和图表完成，不冒充端到端切换耗时。相同内容与路径由 WebPreview 比较后复用 DOM。额外 `repeat-same` 批次每秒重开同一普通文档，3 次、每次 10 秒：原生末次私有提交 **73.2 [73.1–73.2] MiB**，WebView **233.5 [233.3–235.1] MiB**；后者每次进程只报告一次初始 document-readable，没有重新挂载 DOM。这一补测早于最后主批次，保留单独版本与时间记录。

CPU 补测关闭内部 benchmark 循环（`-Passive`），每批 3 次、每次 10 秒，取末段约 3 秒 CPU 时间差。最后批次普通文档为 **1.40% [0.00–1.45] 单逻辑核**，换算 16 核总体约 **0.09%**；空欢迎页为 0.00% [0.00–0.98] 单核。此前同一路径普通文档却测到 **27.52% [20.51–32.89] 单核**（总体约 **1.72%**），同期原生为 **5.16% [3.11–6.24] 单核**（约 **0.32%**）。差异没有可靠归因，最终代码没有保留额外 CPU 优化；**不能据最后较低的一批宣称已改善空闲 CPU，也不能把差异全部解释成内部基准开销**。这是有其他桌面应用活动的单机短测，需在固定后台环境下用 ETW/较长窗口复核。

另外独立试验了父窗口 `WS_CLIPCHILDREN`，普通文档 CPU 为 29.66% [21.51–33.27] 单核，与此前高 CPU 批次的区间重叠，没有证明改善，**已撤回该代码**。测量批次与最后交付构建的 SHA256 分别保留，不把试验版本或旧版测量冒充最终构建数据。

## 回归验收与复现

初次混合实现阶段曾通过 **92 个 Rust 单元/GPUI 测试 + 2 个真实 worker 集成测试**、release 构建、clippy `-D warnings`、fmt、JavaScript 语法检查和 diff 空白检查。真实窗口验证了默认启动的可见预览及文件树、原生编辑、Ctrl+S、切回预览、Ctrl+F 后立即输入、正文定位选区、目录 Focus、窗口缩放和右键菜单关闭恢复；磁盘文件确认原 Markdown 前缀保留，并且新增内容实际写入。WebView DOM 自动化独立覆盖正文/中文 SVG 的直接双击选择及 Ctrl+C（系统剪贴板确认为“开始”）、缩放、拖拽、数学公式、代码高亮、本地图片及图片查看器、搜索、坏图后的正文、安全清洗与协议拒绝越界，还驱动 Ctrl+S 并验证原 Markdown 字节完全保留，以及分栏/预览切换后的实际视口宽度。旧版 GPUI headless 验收曾走原生 fallback；现在该实现已删除，headless 测试只验证宿主/编辑器，阅读必须用真实 WebView 验收。

```powershell
cargo build --release
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --check
node --check assets/web/reader.js
uv run --with playwright python scripts/test-web-preview.py --binary target/release/native-markdown.exe
python scripts/test-delayed-web-preview.py --binary target/release/native-markdown.exe --runs 3
python scripts/make-rendering-fixtures.py
.\scripts\measure-rendering.ps1 -Binary target/release/native-markdown.exe -Output tmp\webview.json -Runs 3 -Seconds 12
# CPU 补测：关闭内部场景驱动循环
.\scripts\measure-rendering.ps1 -Binary target/release/native-markdown.exe -Passive -Scenarios empty,ordinary -Runs 3 -Seconds 10 -Output tmp\passive.json
python scripts/summarize-rendering.py tmp\webview.json
```

无需安装 Playwright 浏览器；它连接应用显式开启的 loopback WebView2 调试端口。自动化只在临时测试文档中工作，端口随该应用进程退出。

后续界面调整：Web 阅读区、代码块和图表视口统一采用 8px 轨道、约 4px 可见圆角滑块、透明背景和无箭头滚动条，并适配深色与高对比模式；目录首次启动默认为 Jump。已重新构建，25 项 app 测试、完整 WebView 功能验收和实际窗口样式检查通过。

## 限制与后续

1. Windows 是唯一已落地阅读平台；Linux/macOS 不再有原生阅读 fallback，未验证跨平台编译或 GUI。安装包、Runtime 缺失机器、首次安装耗时、真正冷启动与低内存硬件尚未实测。
2. Native child 仍有 airspace 限制。原生浮层出现时预览暂时空白是有意取舍，恢复后保持原 WebView 文档；没有引入跨引擎滚动同步。源码和阅读滚动仍各自独立。
3. Mermaid 首次加载有 JS 与字体成本；离屏图表延迟渲染，所以打开多图文档不等于所有图都已执行。没有无界 SVG 缓存或节点编辑/业务 click 事件；新旧 Mermaid 引擎不保证像素布局一致。Web 路径当前拒绝 Mermaid frontmatter（包括仅 title 的 frontmatter），旧原生 title-only 兼容路径已删除。
4. 图表复杂度有预算，但 Mermaid 主线程布局没有硬墙钟中止。超大单图仍可能暂时阻塞 WebView；Rust 窗口与文件状态不在该渲染进程里。长期 soak、全部图表滚动过一遍、图片密集/解压炸弹场景尚需额外压力测试。
5. 原生搜索计数按 Markdown 源映射；懒渲染的 SVG、数学排版、HTML 表格可能与当前 DOM 的可搜文字顺序不同，个别命中只能定位近似可见结果。普通正文、源码定位和已验证选区正常；不宣称实现浏览器式全全文语义搜索。
6. WebView 阅读远程图片保持禁用；目录外图片和 Markdown 链接被拒绝。安全 HTML 表格与 inline HTML 保留，任意脚本/iframe/对象不保留。原生窗口沿用原有固定浅色风格，Web 预览跟随系统深色媒体查询，未新增全应用主题配置。
7. 旧原生图片缓存及其预算已删除；浏览器解码内存仍不受源文件 32 MiB 限制约束。曾实际测试“进入编辑时才装入源码”：3 次大文档宿主仍约 194 MiB，接近此前约 195 MiB，主要差额没有消失；该改动及其额外状态机已撤回。后续应先用堆/布局追踪分摊目录、编辑状态与解析数据的实际成本，不能把宿主全部内存归因于编辑器。
8. 后续用户反馈“空窗口启动后打开 Markdown 卡死闪退”，真实文件选择器复现，并捕获 `gpui/app/async_context.rs:57 RefCell already borrowed` 完整堆栈。此前 12 轮验收主要覆盖命令行直接打开，未覆盖这条路径；仅把构造移出应用实体借用仍不够，外层 `AsyncWindowContext::update` 也持有 GPUI AppCell 借用。WebView2 初始化的嵌套 Windows 消息循环会执行光标/定时异步任务，重入该借用。现在在 `cx.update` 内仅捕获原生父窗口句柄，释放 GPUI 借用后，在同一 UI 线程同步构造 WebView。新增 `test-delayed-web-preview.py`：空窗口启动、定时打开极短 Markdown、确认 document-readable、持续运行并正常退出。旧版本该脚本失败（借用冲突/超时），修复后通过 8 轮（其中 3 轮有另一个阅读器窗口共享默认配置目录）；94 项 Rust 测试、clippy、fmt 和完整 WebView 功能验收再次通过。真实 Ctrl+O 文件选择器也已确认可见正文、目录和文件树。该回归覆盖宿主真实生命周期，不能用 headless 单元测试替代。完整性能矩阵、更多机器与长时间稳定性仍需复核。

## 移除 fork 后的官方版本验证

2026-11-19 已落地：保留 **GPUI 原生外壳 + Wry 0.57.3 / 系统 WebView2 阅读区**，将本地 gpui-component 0.5.1 fork 替换为 crates.io 官方 **gpui-component 0.10.0**，配套固定 **gpui-pre / gpui-pre-platform 0.3.12**。没有切换 Slint，也没有增加前端框架。这里的 gpui-pre 是组件上游要求的发布快照，不是本项目维护的 fork。

### 实际修改与兼容处理

- `Cargo.toml` / `Cargo.lock` 使用 registry 发布包，没有 path、git 或 patch 覆盖；核对了下载的发布归档及提取源码。删除 `vendor/gpui-component` 全部源码，Git 历史仍可用于回退。
- `src/main.rs` 适配平台应用入口；`src/app.rs` 将源码编辑迁移到官方 `EditorState` / `Editor`，查找输入仍使用 `InputState` / `Input`。只启用 Markdown 语法高亮所需的语言特性。
- `src/search_ui.rs` 保存应用自己的 Unicode 搜索范围与摘要背景绘制；`src/source_search.rs` 使用公开文字装饰、显示映射和滚动 API，保留不抢编辑选区的搜索标记和目标居中。过期请求不会滚动新文档，不再向组件库添加 SearchHandle 或搜索方法。
- 定时器、目录扫描使用 GPUI 后台执行器，删除直接 smol 依赖。升级过程中完整测试曾因外部线程唤醒本地任务而异常退出；修复调度路径后重复完整测试通过，没有关闭调度器检查。
- 真实窗口验证发现 WebView 有焦点时原生搜索可能不能接收键盘。现在打开搜索或点击原生控件时交回父 HWND 焦点；Web 页面应用搜索选区后仅发送当前搜索回执，宿主检查请求和打开状态后恢复查找输入。`assets/web/reader.js` 与 `src/app.rs` 一起处理，未增加任意命令 IPC。
- Cargo 包级清理删除 debug/release 组件构建缓存约 **1.6 GiB**；核对旧 fork 的依赖文件已删除。临时对比用旧可执行文件在导出证据后删除。没有修改共享 registry 源码或删除其他项目的共享缓存。

### 本次独立前后实测

这次比较的是 **WebView 版本的旧 fork 与官方组件升级版**，不是再次比较 Native 阅读与 WebView 阅读。旧版 SHA256 `b2fb863297947236c5ddc3ccfa3841598e3ba0dd6abe0acc84a365dd7c764a01`；交付版 `808281e1e7bace3e399665ac89b7bed32d61d0208191b8370751150b0a18735f`。可执行文件分别 27,191,296 / 35,658,240 字节，增加约 **8.1 MiB**，这是磁盘体积而非运行内存。

环境：Windows 10.0.26300、16 个逻辑 CPU、32 GiB RAM，已安装 WebView2 Runtime **157.0.4341.60**。同一机器、文档和临时配置目录，旧版整批完成后再测新版，期间不并行构建或运行另一个阅读器。每场景独立启动 **3 次 × 12 秒**，约每 0.8 秒枚举宿主及后代进程。未清空 OS 文件缓存、未随机交叉运行，不属于严格冷启动；后台桌面负载也未隔离。首次安装 Runtime、跨机器和低内存设备仍未测。

下表为末次采样的**进程树 Private Bytes（私有提交）**，单位 MiB，格式为中位数 [最小–最大]；差值是两组中位数之差。

| 场景 | 升级前 | 升级后 | 差值 |
|---|---:|---:|---:|
| 空欢迎页 | 66.0 [65.9–71.0] | 65.4 [65.1–65.4] | -0.6 |
| 普通 Markdown | 230.4 [229.7–234.1] | 228.8 [224.3–231.4] | -1.6 |
| 多 Mermaid 文档 | 302.5 [298.5–303.4] | 298.1 [298.0–302.5] | -4.4 |
| 大文档（602,110 字节） | 387.2 [386.7–389.9] | 331.7 [329.8–332.7] | -55.5 |
| 多次切换后回到空欢迎页 | 70.4 [70.2–70.8] | 67.4 [67.4–67.5] | -3.0 |

同序场景的末次 **Private Working Set** 中位数分别为升级前 48.8 / 154.4 / 226.3 / 294.8 / 52.9 MiB，升级后 47.5 / 152.4 / 221.5 / 253.2 / 49.5 MiB。Private Bytes 表示私有提交，Private WS 表示驻留的私有页；二者不能混用。Working Set 总和包含多进程共享页，**不是应用独占内存**。每次运行的进程级三个指标、峰值、CPU、时间事件、二进制散列与环境见 [可审阅测量数据](measurements/gpui-upgrade-2026-11-19.json)。完整时间序列保留在本机 `tmp/gpui-upgrade/{before,after}.json`。

例如普通文档第一轮末次采样（并非中位数对应运行），下表为 Private Bytes / Private WS / Working Set，MiB：

| 进程角色 | 升级前 | 升级后 |
|---|---:|---:|
| Rust 宿主 | 68.7 / 51.0 / 98.5 | 67.2 / 49.2 / 99.9 |
| WebView browser | 38.8 / 30.4 / 125.9 | 39.7 / 30.3 / 126.2 |
| crashpad-handler | 2.8 / 1.8 / 10.7 | 2.8 / 1.8 / 10.7 |
| gpu-process | 74.8 / 47.2 / 106.2 | 68.3 / 40.7 / 91.0 |
| network utility | 11.5 / 6.5 / 38.0 | 11.5 / 6.5 / 38.0 |
| storage utility | 7.3 / 2.7 / 19.1 | 7.3 / 2.7 / 19.1 |
| renderer | 30.2 / 19.5 / 68.1 | 27.5 / 16.9 / 65.5 |

启动到 DOM 可读事件的代理时间（不是屏幕首帧测量），同为 ms 中位数 [最小–最大]：

| 场景 | 升级前 | 升级后 |
|---|---:|---:|
| 普通文档 | 674 [649–761] | 643 [634–656] |
| 多图正文 | 706 [682–721] | 778 [651–803] |
| 大文档正文 | 961 [947–969] | 901 [901–906] |

每轮首个可见 Mermaid 的渲染时间分别为 **202.5 [194.6–209.9] → 202.3 [200.5–213.6] ms**，包含首次库加载等成本；其进程启动后完成时间 **916 [878–916] → 993 [847–1006] ms**。只统计各轮首图，未混入后续较快图表。多图夹具共 8 图，惰性渲染使本次每轮只完成视口附近 4 图，不代表全篇图表成本。重复切换场景的 Rust handler 为 **1 [0–2] → 0 [0–0] ms**，不包括 DOM 排版或图表完成，不能作端到端切换时延。

末段约 3 秒 CPU 差分，占**一个逻辑核**的百分比：空页 **1.85 [1.21–2.39] → 2.41 [0.92–2.97]**；普通 **5.94 [4.77–6.53] → 4.63 [3.61–7.00]**；多图 **3.54 [3.01–9.53] → 5.74 [4.72–7.16]**；大文档 **7.61 [4.83–12.24] → 7.17 [7.13–12.40]**；切换后空页 **1.83 [1.21–3.27] → 2.77 [2.30–3.62]**。除以 16 才是整机百分比。本批带场景驱动，不替代前文 Passive 补测；区间重叠且短测有噪声，不宣称空闲 CPU 改善。

结论：在本机和已测夹具上，移除 fork **没有增加进程树内存**，大文档私有提交明显降低，但尚未做堆归因，不将其归功于某一组件实现，也不保证其他文档同幅下降。普通文档与 Mermaid 的内存和首次交互成本基本保持现有 WebView 架构水平；包体增加约 8.1 MiB。结合维护负担消除和功能回归结果，接受这次官方版本升级。它并未消除浏览器进程的既有成本。

### 最终回归结果与复现

- `cargo build --release`、`cargo test` **55 项通过**、`cargo clippy --all-targets -- -D warnings`、fmt 和差异空白检查通过。修复调度问题后，较早的 54 项完整测试另重复通过 3 次并行和 1 次串行；新增当前/过期搜索回执测试后最终为 55 项。
- GPUI 回归验证源码搜索不改变已有选区、离屏目标居中、Unicode/emoji 用户输入、撤销/重做、Ctrl+S 与磁盘精确文本（含原有 CRLF），以及已关闭/过期搜索回执被忽略。
- 实际 Release 的 `test-web-preview.py` 通过：内联 SVG、文字选择、缩放、拖拽、本地图片、KaTeX、高亮、单图失败隔离、清洗、深色主题、保存及阅读/分栏切换。`test-delayed-web-preview.py` 最终 **3/3** 空启动后打开文件可读且正常退出。
- 实际 Windows 窗口通过 Ctrl+O 文件选择器打开 Mermaid 文档；确认 Jump 默认、源码分栏和搜索标记居中；输入 `Diagram 7` 并回车让 WebView 定位后继续输入 `x`，原生搜索正确显示 `Diagram 7x`，验证真实 HWND 键盘路径。
- 真实 IME 候选窗口、完整多 DPI 矩阵、长时间 soak 和其他操作系统未验证；自动注入 Unicode 文本不等于真实 IME 验证。

复现使用前文命令，当前可直接运行 `target/release/native-markdown.exe`。每次升级固定官方版本和锁文件；不要重新引入 vendor 或编辑 registry 源码。更详细的 API 迁移对应关系见 [组件升级说明](gpui-component-fork.md)。

## 阅读滚动时的目录高亮（2026-11-22）

阅读及分栏的预览滚动会高亮当前标题，判定线位于视口顶部下方最多 96px。跟随状态与点击目录的跳转/Focus 状态分开，不移动文档、源码选区或焦点。纯源码模式沿用点击目录的选择状态。目录隐藏、页面隐藏或查看器打开时不采样；静止时不轮询。此功能只维护内存，不增加运行时磁盘读写。

滚动事件通过一个尾随定时器合并，最多每 100ms 检测一次，停下后仍处理末次位置。DOM 更新时索引章节与标题；检测时先二分章节容器，再二分当前章节的标题，避免逐个读取远处标题而破坏 `content-visibility`。只有标题索引变化才向 Rust 发消息并更新目录，旧文档版本和 Focus 范围的迟到消息被忽略。

实际 Release / WebView2 测量：同一构建中交替开关跟随，每组 3 次、每次约 1.58 秒，rAF 每帧滚动 45px，夹具为 61 / 1601 个标题。以下为中位数，CPU 为整个宿主与 WebView2 后代的累计 CPU 毫秒，不是整机 CPU 百分比；帧间隔来自 WebView rAF，不代表原生目录的呈现延迟。

| 场景 | 关闭→开启 CPU ms | 关闭→开启 p95 帧间隔 ms | 开启时消息数 | 开启时几何读取累计 ms |
|---|---:|---:|---:|---:|
| 61 标题 | 312.5→562.5 | 17.0→17.0 | 8 | 0.4 |
| 1601 标题 | 593.8→765.6 | 16.9→16.9 | 9 | 0.3 |
| 61 标题，renderer 4 倍降速 | 2281.3→1765.6 | 16.9→16.9 | 8 | 1.3 |
| 1601 标题，renderer 4 倍降速 | 2187.5→2421.9 | 16.9→16.9 | 8 | 1.2 |

本机短测未观察到 WebView 滚动帧间隔恶化，但原生目录更新存在 CPU 成本，且 CPU 数据有明显噪声（降速小夹具甚至出现开启更低），不能宣称零开销。4 倍降速只模拟 renderer，未降低 Rust/GPU 速度，不能代替真实低端机验证。没有新增内存或磁盘 I/O 的实测结论。

复现：`uv run --with playwright --with psutil python scripts/test-outline-follow.py`，附加 `--cpu-rate 4` 进行 renderer 降速。夹具、配置及 WebView profile 均隔离在临时目录，开发报告写至 `tmp/outline-follow-*-cpu-*.json`。自动化覆盖上下滚动、嵌套标题、引言、静止不轮询、禁用后无几何读取/消息、消息节流以及不触发导航；Rust 回归覆盖重复更新、过期版本、局部标题到全局目录的映射、选区/Focus 不变。

## Windows Debug 启动栈溢出修复（2026-11-22）

全新临时配置下，`target/debug/native-markdown.exe` 空启动可稳定复现 `STATUS_STACK_OVERFLOW (0xc00000fd)`。通过 Windows Debug API / DbgHelp 捕获现场，栈顶是 `__chkstk → NativeMarkdownApp::file_tree_panel → workspace → render`，没有重复递归链；`file_tree_panel` 入口申请 `0xde9b0` 字节（约 890 KiB），而可执行文件的主线程栈保留仅 1 MiB。空页尚未创建 WebView，不能把此崩溃归因于滚动跟随或 IPC。

修复将文件树头部、列表体和外层容器分成独立构建函数，避免未优化构建在同一个栈帧中保留大量链式 UI 构建临时值；没有扩大栈保留，也没有新增运行时配置或日志。`test-delayed-web-preview.py` 同时隔离 APPDATA、TEMP 和 WebView profile，并作为真实 Debug 启动回归运行；GPUI 单元测试和 Release 构建不能代替这项验证。修复后 Debug 启动/延迟打开 3/3 次通过，Release 1/1 次通过；Debug 下 61 / 1601 标题的目录跟随实测通过，68 项 Rust 测试、fmt、clippy 通过。

前述目录跟随性能数据采自本次构建函数拆分之前的 Release，未作为拆分后的性能测量。

## 一手依据

- [Wry 子窗口与平台说明](https://docs.rs/wry/0.57.3/wry/struct.WebViewBuilder.html)
- [WebView2 进程模型](https://learn.microsoft.com/microsoft-edge/webview2/concepts/process-model)、[性能测量建议](https://learn.microsoft.com/microsoft-edge/webview2/concepts/performance)
- [GPUI 上游 WebView 示例的 Windows 合成设置](https://github.com/longbridge/gpui-kit/blob/main/examples/webview/src/main.rs)
- [Tauri 2 进程模型](https://v2.tauri.app/concept/process-model/)、[权限体系](https://v2.tauri.app/security/capabilities/)
- [Mermaid strict 安全设置](https://mermaid.js.org/config/usage.html)、[KaTeX trust 选项](https://katex.org/docs/options)
- [早期 PoC DOMPurify 3.3.1 的安全公告](https://github.com/cure53/DOMPurify/security/advisories/GHSA-h8r8-wccr-v5f2)；最终资源已升级，不以旧 PoC 版本交付。
