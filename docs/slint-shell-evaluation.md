# Slint 替换原生界面的初步评估

调查日期：2026-11-19。此文记录升级前的 GPUI 0.5.1 外壳调研；用户已决定不迁移 Slint，当前官方组件升级状态见 [升级说明](gpui-component-fork.md)。本轮仅核实官方资料和迁移边界；没有修改应用实现、编译 Slint 或运行 Slint GUI，因此没有 Slint 性能实测。

## 结论

Slint 可以作为 GPUI/gpui-component 外壳与简单源码编辑器的替代候选，现有 Wry/WebView2 预览可以继续保留。但这属于重写原生界面层，不是直接替换一个组件依赖。**不能以“现成组件更多”或“必然更省内存”为理由直接迁移**：官方基础组件较齐全，文件树、拖拽分栏和编辑器搜索高亮仍需应用实现或额外组件。合理的下一步是独立小型 PoC，先验证 WebView 子窗口、输入焦点和编辑体验，再做同口径性能比较。

截至调查日，最新稳定版为 **Slint 1.19.0**，发布于 2026-11-16。[官方发布记录](https://github.com/slint-ui/slint/releases/tag/v1.19.0)

## 本项目的替换范围

目标结构是 `Rust 文档/设置逻辑 → Slint 外壳 + Wry/WebView2 阅读区`。Mermaid、数学公式、代码块和正文选择仍由当前浏览器页面承担，换外壳本身不会改善这些能力，也不需要重写它们。

| 实现位置 | 处理方式 |
|---|---|
| `src/app.rs` | 重写 GPUI Entity/Context、布局、工具栏、文件树/目录视图、搜索和编辑器绑定；保留并迁移业务规则 |
| `src/web_preview.rs` | 替换 GPUI 父窗口句柄、canvas 边界、焦点和事件适配；复用 Wry 创建、协议及 IPC 设计 |
| `src/document.rs`、`src/file_tree.rs`、`src/layout_settings.rs`、`src/markdown.rs` | 文件加载/保存、目录数据、设置与 Markdown 分析等 Rust 逻辑优先复用；界面通知部分需适配 |
| `src/web_document.rs`、`assets/web/` | 优先复用现有安全投影、资源加载与 Mermaid/KaTeX/高亮页面 |
| `vendor/gpui-component/`、GPUI 依赖 | 所有界面和编辑器引用完成迁移并验证后，才能整体删除；不能仅替换 Cargo 包名 |

现有源码编辑使用 gpui-component `InputState`，还保留行号、语法高亮、搜索标记与定位能力。采用基础 TextEdit 能满足简单源码编辑，但是否接受这些功能简化必须作为明确的产品取舍。若保留完整编辑体验，则需要额外实现，不能将移除 fork 的收益全部计为维护成本下降。

当前 `src/web_preview.rs` 特意在 GPUI AppCell 借用之外初始化 Wry，原因是 Windows 初始化可能进入嵌套消息循环，之前曾因此在打开文件时崩溃。Slint 版本同样需要避免跨这段调用持有不可重入的可变状态借用。当前为原生子窗口处理的隐藏、恢复和菜单覆盖也需保留对应策略。

## 组件覆盖：哪些现成，哪些仍需实现

| 当前需要 | 官方现有能力 | 迁移判断 |
|---|---|---|
| 工具栏、按钮、输入、设置 | Button、CheckBox、ComboBox、LineEdit、Switch、Slider 等 | 基础控件有现成实现，工具栏布局与业务自行组合 |
| 文件列表、右侧目录 | ListView、StandardListView；ListView 仅实例化可见项 | 扁平列表和目录可做；树结构需维护可见行、展开状态、缩进和键盘语义 |
| 文件树 | 官方标准组件目录未列出 TreeView | 不能按现成完整 TreeView 估算工作量 |
| 拖拽分栏 | 水平/垂直布局；标准目录未列出 Splitter/SplitPane | 需要自定义拖拽手柄、宽度约束与持久化 |
| 表格 | StandardTableView 有列、行、选中与排序回调 | 适合普通数据表；不是成熟可编辑 DataGrid，也不负责 Markdown 表格 |
| 菜单、弹窗 | MenuBar/Menu/MenuItem、ContextMenuArea、PopupWindow、Dialog | 有官方基础设施；与 WebView HWND 的覆盖关系仍需验证 |
| 简单源码编辑 | TextEdit：多行、换行、选中、剪贴板、撤销/重做、edited 回调 | 符合简单编辑基础；中文 IME、大文件、快捷键和保存行为必须实测 |
| 搜索定位 | TextEdit.set-selection-offsets(anchor, focus)，使用 UTF-8 字节偏移；可操作滚动 content-x/y | 当前匹配可通过普通选区表达；多匹配高亮和精确居中无现成等价接口，须单独设计 |
| 代码编辑器 | 标准组件目录未列出 CodeEditor；TextEdit 未列出语法高亮或多匹配标记接口 | 不能将 Slint 开发工具的语法高亮误当作应用 TextEdit 能力 |
| WebView | 标准组件目录未列出稳定 WebView2 控件 | 继续用 Wry 做宿主集成，或承担其他引擎方案的独立验证 |

依据：[标准组件目录](https://docs.slint.dev/latest/docs/slint/reference/std-widgets/overview/)、[ListView 虚拟化](https://docs.slint.dev/latest/docs/slint/reference/std-widgets/views/listview/)、[StandardTableView](https://docs.slint.dev/latest/docs/slint/reference/std-widgets/views/standardtableview/)、[TextEdit API](https://docs.slint.dev/latest/docs/slint/reference/std-widgets/views/textedit/)、[Window 菜单](https://docs.slint.dev/latest/docs/slint/reference/window/window/)。目录中未列出表示本轮未发现官方标准组件，不能推断第三方生态完全没有对应实现。

Slint 从 1.16 起将 Fluent 作为默认样式，其他内置仿原生样式逐步减少维护；因此“原生”主要指编译运行与系统集成，不能理解成每个按钮都是 Win32 原生控件。对 Windows 定位是合适的视觉起点，但 Mac 风格细滚动条仍要按需求定制。[官方默认样式说明](https://slint.dev/blog/default-native-style-change)

## Wry/WebView2 集成：有 API 基础，未实证可交付

官方已经证实的 API：

- `slint::Window::window_handle()` 在启用 `raw-window-handle-06` 后暴露原生窗口/显示句柄；句柄可能要等 `show()` 后事件循环运行才可用。[Window Rust API](https://docs.slint.dev/latest/docs/rust/slint/struct.Window)
- `slint::winit_030::WinitWindowAccessor` 可访问 winit 窗口、注册窗口事件过滤，并异步等待实际窗口创建；要求显式使用 winit 后端。[WinitWindowAccessor](https://docs.rs/i-slint-backend-winit/1.19.0/i_slint_backend_winit/trait.WinitWindowAccessor.html)
- Wry 的 `build_as_child` 接收实现 `HasWindowHandle` 的父窗口；Windows 使用 HWND 子窗口；边界可通过 `with_bounds` / `set_bounds` 管理。[Wry WebViewBuilder](https://docs.rs/wry/0.57.3/wry/struct.WebViewBuilder.html#method.build_as_child)

**由这些 API 推断**，Windows 下 Slint + Wry child WebView2 具有可行的实现路径；本轮没有建立 PoC，不能宣称它已经稳定工作。需要验证：首次显示后延迟创建、WebView2 初始化时消息循环重入、DPI 与物理/逻辑坐标、拖拽分栏实时 resize、Alt/Tab/Ctrl 快捷键、Ctrl+F 焦点往返、中文 IME、弹窗/菜单的 HWND 层叠、隐藏后重新显示、关闭和资源释放。现有 GPUI 曾遇到的问题不会因更换框架自动消失。

官方另有 **Servo 集成示例**。v1.19.0 的 README 已提供可复制的 `webview.slint` 与 WGPU 接入代码，但明确标为实验性、尚不适合生产；它会引入 Servo 引擎，不等于复用系统 WebView2，本次目标没有理由顺带替换已验证的浏览器内核。[固定版本 Servo 示例](https://github.com/slint-ui/slint/blob/v1.19.0/examples/servo/README.md)

## 性能：目前只能确定比较方法

Slint winit 后端支持 Windows，提供 FemtoVG/OpenGL、FemtoVG/WGPU、Skia、软件渲染等选项；这些选项的包体、驱动支持、图形资源和文本表现不同。Skia 官方说明磁盘占用较重；纯软件渲染器官方仍注明文字渲染主要限于西文，不能为追求低资源直接作为中文编辑器的默认推荐。初步 PoC 应优先比较 GPU 后端，并把中文字体、输入法和渲染质量作为门槛。[winit 后端](https://docs.slint.dev/latest/docs/slint/guide/backends-and-renderers/backend_winit/)、[渲染器与限制](https://docs.slint.dev/latest/docs/slint/guide/backends-and-renderers/backends_and_renderers/)

1.18 官方发布说明称大文本编辑与渲染有优化；这只能证明维护方在改进该路径，不能证明本应用迁移后超过当前 GPUI。[1.18 发布说明](https://www.slint.dev/blog/slint-1.18-released)

当前应用普通文档实测整棵进程树 Private Bytes 约 **228.8 MiB**；某轮 host 为 **69.3 MiB**，其余主要为 WebView2 browser/GPU/renderer 等。此数据来自现有项目测量，**不是 Slint 对比结果**。host 还包含文件数据、编辑状态、Wry 宿主和系统库，不能全部算作 GPUI 可消除开销。更换 shell 不会消除 WebView2 的 DOM、JS、GPU 与多进程成本；Slint 自身也有运行时、字体和渲染资源。是否降低总内存、启动时间或空闲 CPU 目前未知。

现有基线来自 [已保存的进程级数据](measurements/webview-only-2026-11-19.json)，Windows build 26300、16 逻辑处理器、预装 WebView2 Runtime，每场景 3 次、每次约 12 秒，未清空操作系统缓存。下表为整个关联进程树的最终 Private Bytes 中位数，不是 Working Set，也不是 Slint 数据：

| 场景 | 当前 GPUI + Wry（MiB） | Slint + Wry |
|---|---:|---|
| 启动未打开文件 | 65.8 | 未测量 |
| 普通文档 | 228.8 | 未测量 |
| 8 个 Mermaid 图表文档 | 305.7 | 未测量 |
| 602,110 字节大文档 | 392.0 | 未测量 |
| 多次切换后回到空闲 | 70.3 | 未测量 |

多图场景沿用当前视口附近的懒渲染，不能当作全部图表完成渲染的成本。较大文档的主进程还承载源码文本与编辑/分析状态；需要分离编辑模式和只读模式测量，才能判断优化来自框架还是数据处理。完整波动、启动指标及测量限制见 [现有渲染评估](markdown-rendering-evaluation.md)。

普通文档第三次快照总计 234.2 MiB，其中 host 69.3、browser 40.5、GPU 72.3、renderer 30.5 MiB，其余为辅助进程；单轮明细与跨轮中位数应分开理解。

独立 PoC 应复用相同 Web 资产、解析与安全协议，保持尺寸、DPI、字体、编辑器内容、Runtime/profile、文档和渲染范围一致，比较空闲、普通、多图、大文档、反复切换回空闲；至少三次记录中位数与范围。统计 host 和全部关联 WebView2 后代的 Private Bytes、Private Working Set、Working Set 明细，WS 总和不能冒充独占内存。补充首次打开可读时间、图表时间、切换延迟、编辑输入延迟和空闲 CPU。没有这些数据，不给“无性能损失”或“更省内存”的保证。

## 许可证

Slint 框架提供 Royalty-free、GPLv3 或商业授权选择，**不是直接以 MIT/Apache 的方式使用框架本体**。Royalty-free 覆盖满足条件的桌面/移动/Web 应用，要求按条款展示 Slint 归属，例如可从顶层菜单访问的 AboutSlint 或公开下载页的徽章；商业授权提供另一条路径。示例和文档使用 MIT，不能由此推断框架同样是 MIT。正式迁移需按最终分发方式核对具体许可证文字。[v1.19.0 LICENSE](https://github.com/slint-ui/slint/blob/v1.19.0/LICENSE.md)、[官方 FAQ](https://github.com/slint-ui/slint/blob/v1.19.0/FAQ.md)、[Royalty-free 原文](https://slint.dev/agreements/slint-royalty-free-license.pdf)

## 本轮边界

完成了官方能力核实和迁移风险梳理；未替换依赖、未移除 fork、未构建或运行 Slint，也没有新增性能结论。此文中的“可以实现”是基于公开 API 的可行性判断，最终是否迁移应由独立 PoC 的交互与同口径测量决定。

建议的 PoC 仅包含工具栏、可展开文件列表、右侧 jump 目录、可拖拽分栏、基础源码编辑与当前 WebView 页面。先验证打开/切换/编辑/保存、中文输入、选区复制、快捷键往返、菜单遮挡与 100/125/150/200% DPI，再使用同一测量脚本比较五种场景。全部通过后再决定整体迁移；本轮不为探索问题提前重写现有应用。
