# 布局与构建缓存维护

## 布局

公共尺寸放在 `ui/design-system.slint` 的 `ProductDesign` 中。尺寸为逻辑像素，不能再次乘系统 DPI。

- 窗口小于 1100 逻辑像素时，页面间距使用 8px；宽窗口使用 12px。
- Modbus 侧栏在 260–300px 内变化，串口侧栏在 250–280px 内变化。
- CAN 通道配置使用最小/首选宽度及伸缩比例，备注通过 GridLayout 分配剩余宽度。
- CAN 主界面树栏显示宽度不超过窗口的 30%，保留用户拖拽宽度；窗口放大后可恢复。尺寸变化不会覆盖保存的偏好。
- 依赖窗口宽度的布局尺寸在 `changed width` 中更新，避免窗口隐式尺寸与子布局形成绑定循环。
- 长表格保留水平滚动，长标签允许换行；不要通过缩小所有文字来适应窄窗口。
- 修改共享组件后，检查 CAN、通道配置、串口、Modbus，分别覆盖最小宽度、宽窗口、中英文、100%/200% DPI。编译检查与截图检查都要执行。

## 缓存

预览清理候选：

```powershell
.\scripts\prune-build-cache.ps1
```

执行清理：

```powershell
.\scripts\prune-build-cache.ps1 -Apply
```

默认清理超过七天的旧增量单元，每个包至少保留最近一个单元；保留依赖、build-script 输出、可执行文件及正式 Release 缓存。另清理明确废弃的 `artifacts/target-release-no-lto` 和 vendor 内部 target。

脚本默认仅预览，构建期间拒绝执行，检查目标范围和重解析点。删除的文件为可再生成缓存，不进回收站，恢复方式为重新编译。不要为了节省空间每次执行 cargo clean，否则会丢失可复用依赖。

2026-09-05 本次清理删除约 55.75 GiB，明细在 `artifacts/cache-cleanup-report.json`。Slint 补丁源码、驱动、用户工程、安装包和测试报告保留。
