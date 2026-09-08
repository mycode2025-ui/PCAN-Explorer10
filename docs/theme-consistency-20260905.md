# UI 主题一致性整理（2026-09-05）

## 本轮修改

- CAN 的 TBtn、Modbus 的 AccentButton/ToolAction、串口的 ToolButton 使用同一 ProductButton 基础组件；保留页面回调。
- 统一按钮的悬停、按下、选中、禁用和焦点样式。鼠标、键盘、无障碍激活统一经过 enabled 检查，修复禁用按钮的无障碍入口绕过禁用状态问题。
- 共享字号、紧凑控件高度、弹窗间距令牌；通道配置按共享字号和表单高度调整。
- 关闭、排序、通道下拉箭头使用 SVG，避免依赖字体字形；修复下拉箭头垂直对齐。
- 深色主题的主按钮使用深色文字，提高浅蓝背景上的文字可读性；品牌页头白字保持不变。
- 发送帮助、主界面详情、Modbus 设置弹窗接入 ProductModal，统一 Esc 关闭入口。
- 补齐 Modbus 设置弹窗常用标题、字段、操作和提示的中文翻译，以及 CAN 筛选标签与 OTA 常用操作翻译。协议标识、部分功能码列表仍保留英文。

## 验证范围

- Slint viewer 静态检查和无窗口渲染：CAN 发送窗口、通道配置、Modbus、串口；浅色/深色及窄窗口样例。
- 按钮测试夹具调用禁用与正常按钮的 activate，计数为 1，符合预期。
- 通道配置 150% 渲染、按钮 200% 渲染及紧凑模式夹具。不是实际切换 Windows 显示器 DPI 的交互测试。
- 图像与夹具位于 artifacts/theme-review；未连接硬件、未执行安装或版本更新。
- 原生窗口重开、键盘/屏幕阅读器实际事件及多显示器拖动尚未人工验证；不能将静态渲染视为这些交互测试通过。
- Rust 回归：`cargo test -p pcanwork --bin pcanwork`，54 通过、0 失败、1 联网测试按原设置忽略；测试构建 3 分 50 秒，执行 2.95 秒。
- `cargo check -p modbus-tools -p serial-tool` 通过，51.98 秒。本轮涉及共享 UI，以上为开发/测试构建时间，不是 Release 安装包耗时。
- 最后一次微调后的三程序联合 `cargo check -p pcanwork -p modbus-tools -p serial-tool` 通过，2 分 32 秒。
