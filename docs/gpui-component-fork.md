# gpui-component fork 维护说明

Native Markdown 使用个人 fork 提供三个通用的 `TextView` 扩展，但 Mermaid 解析、worker、SVG 校验、图片查看器和图片缓存仍属于应用。本 fork 应保持为一组可重复移植的薄补丁，而不是 gpui-component 的长期分叉。

## 当前基线

- Fork：`https://github.com/zaopuppy/gpui-component`
- 维护分支：`native-markdown-textview`
- 上游 0.5.1 基线：`0f0ab35233212f8f3277028995caf0c41e13ee6c`
- 一次性标题跳转：`97cdfe86`
- 自定义代码块渲染：`f8b48e94`
- 图片激活回调：`46f6b9c4`
- 应用通过 `Cargo.toml` 和 `Cargo.lock` 固定完整 commit，不跟踪可移动分支。

依赖使用 HTTPS 地址，保证普通构建机不需要 GitHub SSH key；维护者仍可用 SSH remote 推送。

## 模块边界

Fork 只提供三个窄接口：

- `scroll_to_heading_once(heading_index, request_id)`：将 Markdown 标题映射到虚拟列表项，只在 request id 变化时滚动一次。
- `code_block_renderer(...)` 与 `CodeBlock::source_range()`：允许应用替换某类代码块的正文，并保留原始 Markdown 字节范围。
- `image_activation_handler(...)` 与 `ImageInfo`：报告鼠标或键盘激活的图片；返回 `false` 时保留链接图片的默认打开行为。

不要把 Merman、Mermaid URI、Native Markdown 状态或产品 UI 放入 fork。这样 TextView 仍是通用模块，应用通过 Adapter 接口接入 Mermaid，三个补丁也能独立移植或删除。

## 升级上游版本

1. 在 fork 克隆中更新上游引用：

   ```powershell
   git fetch upstream --tags
   git switch -c native-markdown-textview-<version> <upstream-release-commit>
   ```

2. 按顺序移植补丁：

   ```powershell
   git cherry-pick 97cdfe86 f8b48e94 46f6b9c4
   ```

   如果上游 API 已经覆盖同一能力，优先迁移应用并删除对应补丁，不保留重复接口。

3. 在 fork 中验证：

   ```powershell
   cargo test -p gpui-component --lib
   ```

4. 推送新维护分支，取得最终完整 commit hash。不要在应用里 pin 分支名。

5. 更新应用的 `Cargo.toml` 中 `rev`，然后刷新锁文件并检查依赖来源：

   ```powershell
   cargo update -p gpui-component
   cargo tree -d
   cargo test
   ```

   `gpui` 必须继续与应用使用同一 crates.io 版本和来源；同版本但不同 Git 来源也会形成不兼容的 Rust 类型。

6. 用实际 Mermaid 文档运行 release 基准：

   ```powershell
   .\scripts\benchmark-memory.ps1 `
     'E:\work\docs\chromium\chrome-actor-mechanism.md' `
     -Scenario all `
     -Seconds 3
   ```

## 日常规则

- 每个通用能力一个独立 commit；不要把上游同步、格式化和功能修改混在一起。
- 推送前保持 fork 工作树干净，并运行 gpui-component 完整库测试。
- 应用升级 rev 时单独提交 `Cargo.toml` 与 `Cargo.lock`，便于 bisect 和一键回退。
- 回退应用只需恢复前一个 rev 和锁文件；不要 force-push 已被应用 pin 的 commit。
- 定期检查上游是否出现等价扩展点。一旦可用，迁移到上游 API 并退休 fork 补丁。
