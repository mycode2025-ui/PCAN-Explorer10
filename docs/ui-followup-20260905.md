# UI 一致性遗留项修复

本轮针对五项复查结果，不变更版本、不打包、不连接硬件。

1. Modbus 自定义表格、日志、卡片和弹窗颜色统一引用 ProductDesign。原生输入组件继续使用 Slint 原生控件，Palette 仅保留色系设置。
2. 新增 ProductDialogSurface，将 Modbus 设置弹窗限制在窗口内，并保留至少 16px 外边距；窗口不足时滚动显示原尺寸内容。
3. Modbus 和串口页头加入共享“紧凑/舒适”切换，联动共享按钮、字体和表单/行高度；CAN 原有入口保留。不包含跨进程设置同步或新增持久化。
4. 补齐扫描、记录频率、仿真模式、数值标签、计算说明、状态和地址表头的中文。功能码、字节序、协议缩写不更改其业务含义。
5. ProductModal 使用 PopupWindow 管理焦点，手动控制开闭。用于 CAN 详情、发送帮助及 Modbus 设置/授权弹窗。Tab 导航保持在弹窗内，关闭后由框架恢复原焦点。

## 验证

- 新增 modal_keyboard 自动测试：正向 Tab、Shift+Tab、Esc 关闭、焦点恢复、重复打开两次，全部通过。
- 此键盘测试使用 Slint 软件窗口和真实 WindowEvent 分发，不连接设备、不打开可见窗口；不等于完成 Windows 多显示器人工交互测试。
- 所有 CAN Slint 文件、Modbus 和串口通过 viewer 静态检查。
- 已查看浅色弹窗、深色 Modbus/串口和 820×560 扫描弹窗渲染；扫描弹窗保留外边距并出现垂直滚动条。
- 渲染样例：artifacts/theme-review/modbus-scan-small.png、modbus-dark-new.png、serial-dark-new.png。
- 最终三程序联合 `cargo check -p pcanwork -p modbus-tools -p serial-tool` 通过，耗时 2 分 17 秒（开发检查，不是 Release 打包）。
