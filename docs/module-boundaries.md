# 主程序与后端职责拆分

本轮将原有实现移动到独立 Rust 模块，通过原父模块转接，保留调用方 API。没有调整收发算法、厂商驱动选择、协议格式或安装版本。

## PcanWork

| 文件 | 职责 |
| --- | --- |
| src/main.rs | 共享应用状态、数据类型、窗口辅助代码与入口 |
| src/startup.rs | 应用初始化、任务接入和主事件循环 |
| src/channel_management.rs | 通道编辑、硬件扫描与稳定身份匹配 |
| src/ipc_dispatch.rs | 快照发布、IPC 请求校验与分发、订阅广播 |
| src/project_state.rs | 工程设置收集、保存、恢复及筛选解析 |
| src/playback_view.rs | 回放文件列表与通道映射 |
| src/python_output.rs | Python 子进程输出及结束处理 |
| src/ui_refresh.rs | 界面模型同步和刷新诊断（上轮拆出） |

## CAN 后端

| 文件 | 职责 |
| --- | --- |
| src/can.rs | 公共帧/配置/命令/事件类型、队列基础设施与调度辅助 |
| src/can/pcan.rs | PEAK PCAN 枚举、FFI、初始化和收发 |
| src/can/vci.rs | 既有 VCI 适配器、设备共享和 PnP 检查 |
| src/can/zcan.rs | ZLG 配置验证、枚举、ZCAN FFI、设备共享和收发 |
| src/can/controller.rs | 控制器事件循环 |
| src/can/send_queue.rs | 有界发送队列（上轮拆出） |

CAN 源码由 pcanwork-core 通过 path 引用，子模块使用显式 path 保持解析位置正确。保留原 can:: 公共入口，调用方不需要修改路径。

## Modbus

| 文件 | 职责 |
| --- | --- |
| modbus/src/backend.rs | 公共配置/命令、任务装配、传输及流量监控、扫描与测试 |
| modbus/src/backend/master.rs | 主站轮询、写入、曲线和记录 |
| modbus/src/backend/slave.rs | 从站服务、寄存器、仿真与请求响应 |
| modbus/src/backend/ui_bridge.rs | 窗口缓存及跨线程界面同步 |
| modbus/src/backend/grid.rs | 表格行及派生值显示（上轮拆出） |

父模块仍负责共享类型与内部导入，内部接口使用 pub(super)。这是同一 crate 内的职责拆分，不等同于独立编译单元，也不据此承诺编译时间下降。

## 本轮结果

三个原文件从本轮开始的 5795 / 5118 / 5267 行降至 3025 / 1658 / 2610 行。新模块最大约 1421 行。原文件保留相关回归测试，没有通过迁走测试来降低这些数字。

工作区 cargo check 通过。PcanWork、pcanwork-core、Modbus 联合回归 172 项通过，5 项依赖硬件、外部资源或长期运行的测试保持忽略。两个依赖工程根目录的既有版本号测试在根目录单独运行，均通过，总计 174 项通过。
