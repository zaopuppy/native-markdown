# GPUI Component：已切回官方发布版本

更新：2026-11-19。应用使用 crates.io 的 `gpui-component = "=0.10.0"`，配套 `gpui-pre = "=0.3.12"`、`gpui-pre-platform = "=0.3.12"`。没有 path/git/patch 覆盖，不修改 Cargo 共享缓存。`vendor/gpui-component` 的源码已移除；Git 历史保留回退依据。

这里的 gpui-pre 是组件上游所依赖的发布快照，不是本项目维护的 GPUI fork；本项目只固定并使用其发布版本。Wry 0.57.3 和系统 WebView2 继续承担 Markdown 阅读，不引入 Slint。

## 剩余定制如何移出依赖

| 原本地补丁 | 当前实现 |
|---|---|
| TextView 的标题跳转、代码块替换、图片激活、正文搜索 | 随旧阅读器删除；由 `assets/web/` 和 `src/web_preview.rs` 提供浏览器阅读体验 |
| Unicode 搜索范围、摘要文字背景 | `src/search_ui.rs` 中的应用组件，仅使用 GPUI 公开绘制 API；`src/markdown.rs` 复用同一匹配规则 |
| InputState 外部搜索标记 | `src/source_search.rs` 调用官方 `EditorState::create_decorations_collection`，独立于编辑选区 |
| 搜索目标滚动、居中 | 通过 `display_map`、`range_to_bounds`、`input_bounds`、`line_height`、`set_scroll_offset` 完成；视口外先定位显示行，再用实际布局修正；先展开遮挡目标的折叠 |
| SearchHandle 绘制几何 | 应用层测试记录，不再加入组件库 |
| 本地删除上游 Markdown/HTML 模块以裁剪依赖 | 不再维护此类补丁。官方库有内置解析依赖，不表示应用启用另一套 Markdown 阅读器 |

源码编辑改用 `EditorState` / `Editor`，查找框仍用 `InputState` / `Input`。只启用源码 Markdown 高亮所需的 `tree-sitter-markdown`，没有开启所有语言集合。文件管理、原子保存、恢复、设置和 Web 内容安全边界继续由现有 Rust 模块负责。

焦点和编辑选区仍分别管理：源码匹配标记不会移动编辑光标或替换用户选区，滚动修正带请求校验，已过期的延迟操作不会滚动到新文档。摘要使用实际文字布局绘制背景，保持 Unicode、换行和字形塑形。

## 升级中处理的兼容差异

- 新版 `set_value` 是程序赋值，不再发出 `InputEvent::Change`。加载文档继续显式更新文档状态；模拟用户编辑的测试使用用户编辑路径，不依赖旧的赋值副作用。
- 窗口由 `gpui_platform::application()` 创建；焦点 API 和滚轮 API 适配新版签名。分栏测试按新版包含边框的布局坐标操作，保留同样的鼠标跟随断言。
- 定时器和目录扫描改用 GPUI 后台执行器，删除应用直接依赖的 smol。原有 smol 外部线程唤醒会触发新版确定性测试调度器检查，日志曾出现异步线程丢弃本地任务并异常退出；不通过关闭检查或忽略测试绕过。
- Win32 原生焦点与 GPUI 逻辑焦点不同。点击原生控件、打开搜索时将 HWND 焦点交回父窗口；WebView 设置搜索选区后通过限定的 `search-applied` 回执恢复搜索框焦点。页面仅对原生搜索发出回执，宿主只接受当前请求且搜索仍处于打开和已定位状态的情况，不提供任意 Rust 命令。

## 验证与性能

最终命令、功能结果、五场景进程树测量与包体变化见 [Markdown 渲染评估](markdown-rendering-evaluation.md) 的“移除 fork 后的官方版本验证”一节。测量覆盖所有关联 WebView2 进程，不能仅比较 Rust 主进程，也不能把 Working Set 求和当作独占内存。

手动 GUI 检查与自动化测试均使用实际 Release 程序。GPUI 测试可覆盖逻辑焦点、选区、布局和文本保存，但不能单独证明 HWND 焦点或实际输入法候选窗正常；相关限制需与运行结果一起记录。

## 升级依据

此前逐文件比较了 vendor 与官方 0.5.1：大多数补丁用于已删除的 Markdown 阅读器，真正阻止直接替换的是源码的非侵入搜索标记。官方 0.5.1 的光标 API 会改变选区并聚焦编辑器，而滚动接口为私有，因此选择升级到已提供独立文字装饰和滚动 API 的版本。

后续升级固定发布版本并更新锁文件，通过回归与实际 WebView 验证；不要重新引入本地组件 fork 或编辑 registry 源码。
