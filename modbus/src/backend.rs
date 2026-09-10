//! Async backend: a Tokio runtime on a dedicated thread driving the Modbus
//! master (poll) and slave (simulator) engines, bridged to the Slint UI through
//! a weak handle and a command channel.
//!
//! The master side is MDI-style: any number of independent poll windows, each
//! with its own connection, engine, display settings and cached state. The flat
//! UI properties mirror the *active* window; switching windows restores the
//! target window's config and last-known state from its cache.

use crate::format::*;
use crate::protocol::*;

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::future::{ready, Ready};
use std::io::Write as _;
use std::net::ToSocketAddrs;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use tokio_modbus::client::Context;
use tokio_modbus::prelude::*;
use tokio_modbus::server::Service;
use tokio_serial::SerialStream;

const RESPONSE_TIMEOUT_MS: u64 = 1000;
const SCAN_TIMEOUT_MS: u64 = 400;
const COMMAND_QUEUE_CAPACITY: usize = 512;
const TRAFFIC_QUEUE_CAPACITY: usize = 1024;
const ENGINE_CONTROL_QUEUE_CAPACITY: usize = 256;
const CHART_LEN: usize = 600;
const CHART_SERIES_PER_PAGE: usize = 12;
const CHART_COLORS: [u32; 12] = [
    0xFF61AFEF, 0xFF98C379, 0xFFE06C75, 0xFFE5C07B, 0xFFC678DD, 0xFF56B6C2, 0xFFD19A66, 0xFFABB2BF,
    0xFFE06CB4, 0xFF7FB069, 0xFF5C9DFF, 0xFFF0A030,
];

// ===========================================================================
// Public API
// ===========================================================================

/// Snapshot of the flat master UI config, stored per window so it can be
/// restored when the window is re-selected.
#[derive(Clone, Default)]
pub struct UiCfg {
    pub transport: i32,
    pub host: String,
    pub port: i32,
    pub serial: String,
    pub baud_index: i32,
    pub databits_index: i32,
    pub parity: i32,
    pub stopbits: i32,
    pub slave_id: i32,
    pub function: i32,
    pub address: i32,
    pub quantity: i32,
    pub scanrate: i32,
    pub format: i32,
    pub scl_enabled: bool,
    pub scl_x1: String,
    pub scl_y1: String,
    pub scl_x2: String,
    pub scl_y2: String,
    pub scl_decimals: i32,
    pub col_normal: i32,
    pub col_op1: i32,
    pub col_v1: String,
    pub col_c1: i32,
    pub col_op2: i32,
    pub col_v2: String,
    pub col_c2: i32,
    pub vn_enabled: bool,
    pub vn_text: String,
}

pub struct MasterCfg {
    pub transport: Transport,
    pub slave_id: u8,
    pub area: Area,
    pub address: u16,
    pub quantity: u16,
    pub scan_ms: u64,
    pub format: RegFormat,
    pub scaling: Scaling,
    pub colors: ColorRules,
    pub value_names: ValueNames,
    /// false when the selected function is a WRITE code — the engine connects but
    /// does not poll (the main grid is then a write definition, not read display).
    pub poll: bool,
    /// Some → wrap the TCP connection in TLS (Modbus/TCP Security).
    pub tls: Option<crate::tls::TlsClientCfg>,
    /// 单次请求响应超时(ms)。0 → 用默认 1000ms。
    pub timeout_ms: u64,
    /// 通信错误(超时/断开)后是否自动重连。
    pub reconnect: bool,
    /// 自动重连间隔(ms)。
    pub reconnect_ms: u64,
}

pub struct SlaveCfg {
    pub transport: Transport,
    pub unit_id: u8,
    pub ignore_unit_id: bool,
    pub area: Area,
    pub address: u16,
    pub quantity: u16,
    pub format: RegFormat,
    /// Language selected when the server is started; used for asynchronous status text.
    pub english: bool,
    /// Some → accept TLS connections (Modbus/TCP Security).
    pub tls: Option<crate::tls::TlsServerCfg>,
}

/// Register-value simulation for the slave: animate a block of registers/coils.
#[derive(Clone)]
pub struct SimCfg {
    pub mode: SimMode,
    pub area: Area,
    pub address: u16,
    pub quantity: u16,
    pub step: u16,
    pub min: i64,
    pub max: i64,
    pub interval_ms: u64,
    pub target: i32, // -1 = whole block; else the single absolute address to animate
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SimMode {
    Off,
    Increment,
    Decrement,
    Random,
    Toggle,
}

impl SimMode {
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => SimMode::Increment,
            2 => SimMode::Decrement,
            3 => SimMode::Random,
            4 => SimMode::Toggle,
            _ => SimMode::Off,
        }
    }
}

#[derive(Clone, Copy)]
pub enum WriteFunc {
    SingleCoil,
    SingleReg,
    MultiCoils,
    MultiRegs,
}

impl WriteFunc {
    pub fn from_index(i: i32) -> Self {
        match i {
            0 => WriteFunc::SingleCoil,
            2 => WriteFunc::MultiCoils,
            3 => WriteFunc::MultiRegs,
            _ => WriteFunc::SingleReg,
        }
    }
}

/// A user-defined derived channel: a named formula evaluated over the polled
/// register window (r0 = first register).
#[derive(Clone)]
pub struct DerivedCh {
    pub name: String,
    pub formula: String,
}

pub struct WriteReq {
    pub func: WriteFunc,
    pub address: u16,
    pub text: String,
    /// If set, `text` is a single number encoded across registers (FC16) using
    /// this format's width and byte order (32/64-bit int/float).
    pub encode: Option<RegFormat>,
}

/// One entry in the client Write List: a holding register, a value, and whether
/// it auto-increments each cycle during periodic auto-write.
#[derive(Clone)]
pub struct WriteItem {
    pub address: u16,
    pub value: u16,
    pub inc: bool,
}

pub struct ScanCfg {
    pub transport: Transport,
    pub slave_id: u8,
    pub area: Area,
    pub start: u16,
    pub count: u16,
}

pub struct SlaveScanCfg {
    pub transport: Transport,
    pub area: Area,
    pub address: u16,
    pub from_id: u8,
    pub to_id: u8,
}

#[derive(Clone)]
pub struct LogCfg {
    pub path: String,
    pub each_read: bool,
    pub period_s: u32,
    pub delimiter: char,
    pub on_change: bool,
    pub timestamp: bool,
}

pub enum Cmd {
    NewWindow(UiCfg),
    SelectWindow {
        id: u32,
        current: UiCfg,
    },
    CloseWindow {
        id: u32,
    },
    SetFloat {
        id: u32,
        float: Option<slint::Weak<crate::PollFloat>>,
    },
    MasterConnect(MasterCfg),
    MasterDisconnect,
    MasterWrite(WriteReq),
    MasterWriteOnce {
        func: WriteFunc,
        items: Vec<WriteItem>,
    },
    MasterAutoWrite {
        func: WriteFunc,
        items: Vec<WriteItem>,
        interval_ms: u64,
    },
    MasterAutoWriteStop,
    MasterMaskWrite {
        address: u16,
        and_mask: u16,
        or_mask: u16,
    },
    MasterReadWrite {
        read_addr: u16,
        read_qty: u16,
        write_addr: u16,
        write_values: Vec<u16>,
    },
    MasterFormat(RegFormat),
    MasterName {
        address: u16,
        name: String,
    },
    MasterScaling(Scaling),
    MasterColors(ColorRules),
    MasterValueNames(ValueNames),
    MasterCellFormat {
        address: u16,
        format: Option<RegFormat>,
    },
    MasterDerived(Vec<DerivedCh>),
    MasterChartExport(String),
    MasterChartAxis {
        addr: u16,
        right: bool,
    },
    MasterChartPage(i32),
    MasterChartFocus(u16),
    MasterReadDef {
        area: Area,
        address: u16,
        quantity: u16,
        scan_ms: u64,
        poll: bool,
    },
    MasterStartLog(LogCfg),
    MasterStopLog,
    ScanAddress(ScanCfg),
    ScanSlave(SlaveScanCfg),
    ScanStop,
    SlaveStart(SlaveCfg),
    SlaveStop,
    SlaveEdit {
        address: u16,
        text: String,
    },
    SlaveEditAt {
        area: Area,
        address: u16,
        text: String,
    },
    SlaveName {
        address: u16,
        name: String,
    },
    SlaveCellFormat {
        address: u16,
        format: Option<RegFormat>,
    },
    /// 导出全部数据到 CSV。server=true: 服务端扫 4 表。
    /// client: active_csv = UI 端活动窗口的当前行(保证名字/功能码正确, 因为未连接窗口
    /// 的预览数据只在 UI), 其余窗口从后端 cache 取(已连接/已轮询的)。
    ExportCsv {
        path: String,
        server: bool,
        active_id: u32,
        active_csv: String,
    },
    SlaveView {
        area: Area,
        address: u16,
        quantity: u16,
        format: RegFormat,
    },
    SlaveScaling(Scaling),
    SlaveColors(ColorRules),
    SlaveValueNames(ValueNames),
    SlaveSimStart(SimCfg),
    SlaveSimStop,
    SlaveAutoInc {
        address: u16,
    },
}

#[derive(Clone)]
pub struct CommandSender {
    tx: mpsc::Sender<Cmd>,
    weak: slint::Weak<crate::AppWindow>,
    rejected: Arc<AtomicU64>,
    high_watermark: Arc<AtomicUsize>,
    last_report_ms: Arc<AtomicU64>,
}

#[derive(Clone, Copy, Debug)]
pub struct CommandRejected;

impl CommandSender {
    pub fn send(&self, command: Cmd) -> Result<(), CommandRejected> {
        let result = self.tx.try_send(command);
        let rejected = result.is_err();
        if rejected {
            self.rejected.fetch_add(1, Ordering::Relaxed);
        } else {
            self.high_watermark
                .fetch_max(self.queue_depth(), Ordering::Relaxed);
        }
        self.report_health(rejected);
        result.map_err(|_| CommandRejected)
    }

    fn queue_depth(&self) -> usize {
        self.tx.max_capacity().saturating_sub(self.tx.capacity())
    }

    fn report_health(&self, force: bool) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let previous = self.last_report_ms.load(Ordering::Relaxed);
        if !force && now_ms.saturating_sub(previous) < 500 {
            return;
        }
        if self
            .last_report_ms
            .compare_exchange(previous, now_ms, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let depth = self.queue_depth();
        let high = self.high_watermark.load(Ordering::Relaxed);
        let rejected = self.rejected.load(Ordering::Relaxed);
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            app.set_runtime_health(
                format!("CMD {depth}/{COMMAND_QUEUE_CAPACITY} H{high} R{rejected}").into(),
            );
            app.set_runtime_loss(rejected > 0);
        });
    }
}

pub fn start_backend(weak: slint::Weak<crate::AppWindow>) -> CommandSender {
    let (tx, rx) = mpsc::channel(COMMAND_QUEUE_CAPACITY);
    let sender = CommandSender {
        tx,
        weak: weak.clone(),
        rejected: Arc::new(AtomicU64::new(0)),
        high_watermark: Arc::new(AtomicUsize::new(0)),
        last_report_ms: Arc::new(AtomicU64::new(0)),
    };
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");
        rt.block_on(controller(rx, UiSink { weak }));
    });
    sender.report_health(true);
    sender
}

// ===========================================================================
// Controller — owns the poll windows, slave and scan lifecycles.
// ===========================================================================

struct Win {
    id: u32,
    title: String,
    cfg: UiCfg,
    connected: bool,
    engine: Option<MasterHandle>,
    cache: Arc<Mutex<WinCache>>,
    float: Arc<Mutex<Option<slint::Weak<crate::PollFloat>>>>,
}

/// UI 功能码索引(0-7)→ 数据表(与 main.rs::func_area 一致)。
fn func_area_be(function: i32) -> Area {
    match function {
        0 | 4 | 6 => Area::Coils,
        1 => Area::DiscreteInputs,
        3 => Area::InputRegisters,
        _ => Area::HoldingRegisters,
    }
}

/// CSV 字段转义(同 main.rs::csv_escape)。
fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn save_cfg(windows: &mut [Win], id: u32, cfg: UiCfg) {
    if let Some(w) = windows.iter_mut().find(|w| w.id == id) {
        w.cfg = cfg;
    }
}

fn push_windows(windows: &[Win], ui: &UiSink) {
    let tabs: Vec<crate::WinTab> = windows
        .iter()
        .map(|w| crate::WinTab {
            id: w.id as i32,
            title: w.title.clone().into(),
            connected: w.connected,
        })
        .collect();
    ui.set_windows(tabs);
}

fn show_window(windows: &[Win], id: u32, ui: &UiSink) {
    if let Some(w) = windows.iter().find(|w| w.id == id) {
        ui.set_active_win(id);
        ui.push_config(&w.cfg);
        ui.push_cache(&w.cache.lock().unwrap());
    }
}

async fn controller(mut rx: mpsc::Receiver<Cmd>, ui: UiSink) {
    let active = Arc::new(AtomicU32::new(1));
    let mut windows: Vec<Win> = vec![Win {
        id: 1,
        title: "Poll 1".into(),
        cfg: UiCfg::default(),
        connected: false,
        engine: None,
        cache: Arc::new(Mutex::new(WinCache::default())),
        float: Arc::new(Mutex::new(None)),
    }];
    let mut next_id = 2u32;
    let mut slave: Option<SlaveHandle> = None;
    let mut scan: Option<JoinHandle<()>> = None;

    push_windows(&windows, &ui);

    macro_rules! to_active {
        ($msg:expr) => {{
            let aid = active.load(Ordering::Relaxed);
            if let Some(w) = windows.iter().find(|w| w.id == aid) {
                if let Some(e) = &w.engine {
                    e.send($msg);
                }
            }
        }};
    }

    while let Some(cmd) = rx.recv().await {
        // 处理命令前，把各窗口 tab 的连接态与异步 cache 对账：连接失败/掉线会异步写 cache，
        // 此处同步到 tab 并重推，避免标签停留在过时的"已连接"(成功连接仍由乐观置位即时变绿)。
        {
            let mut changed = false;
            for w in windows.iter_mut() {
                let c = w.cache.lock().unwrap().connected;
                if c != w.connected {
                    w.connected = c;
                    changed = true;
                }
            }
            if changed {
                push_windows(&windows, &ui);
            }
        }
        match cmd {
            Cmd::NewWindow(current) => {
                save_cfg(
                    &mut windows,
                    active.load(Ordering::Relaxed),
                    current.clone(),
                );
                let id = next_id;
                next_id += 1;
                windows.push(Win {
                    id,
                    title: format!("Poll {id}"),
                    cfg: current,
                    connected: false,
                    engine: None,
                    cache: Arc::new(Mutex::new(WinCache::default())),
                    float: Arc::new(Mutex::new(None)),
                });
                active.store(id, Ordering::Relaxed);
                push_windows(&windows, &ui);
                show_window(&windows, id, &ui);
            }
            Cmd::SelectWindow { id, current } => {
                save_cfg(&mut windows, active.load(Ordering::Relaxed), current);
                active.store(id, Ordering::Relaxed);
                show_window(&windows, id, &ui);
                push_windows(&windows, &ui);
            }
            Cmd::SetFloat { id, float } => {
                if let Some(w) = windows.iter().find(|w| w.id == id) {
                    *w.float.lock().unwrap() = float.clone();
                    if let Some(fw) = float {
                        push_cache_to_float(&fw, &w.title, &w.cache.lock().unwrap());
                    }
                }
            }
            Cmd::CloseWindow { id } => {
                if windows.len() > 1 {
                    if let Some(pos) = windows.iter().position(|w| w.id == id) {
                        if let Some(e) = windows[pos].engine.take() {
                            e.send(MasterMsg::Stop);
                        }
                        windows.remove(pos);
                        if active.load(Ordering::Relaxed) == id {
                            let nid = windows[0].id;
                            active.store(nid, Ordering::Relaxed);
                            show_window(&windows, nid, &ui);
                        }
                        push_windows(&windows, &ui);
                    }
                }
            }
            Cmd::MasterConnect(cfg) => {
                let aid = active.load(Ordering::Relaxed);
                if let Some(w) = windows.iter_mut().find(|w| w.id == aid) {
                    if let Some(e) = w.engine.take() {
                        e.send(MasterMsg::Stop);
                    }
                    let sink =
                        ui.window_sink(aid, active.clone(), w.cache.clone(), w.float.clone());
                    w.engine = Some(spawn_master(cfg, sink));
                    w.connected = true;
                }
                push_windows(&windows, &ui);
            }
            Cmd::MasterDisconnect => {
                let aid = active.load(Ordering::Relaxed);
                if let Some(w) = windows.iter_mut().find(|w| w.id == aid) {
                    if let Some(e) = w.engine.take() {
                        e.send(MasterMsg::Stop);
                    }
                    w.connected = false;
                    {
                        let mut c = w.cache.lock().unwrap();
                        c.connected = false;
                        c.status = "Disconnected".into();
                    }
                    ui.master_status("Disconnected", false);
                }
                push_windows(&windows, &ui);
            }
            Cmd::MasterWrite(req) => to_active!(MasterMsg::Write(req)),
            Cmd::MasterWriteOnce { func, items } => {
                to_active!(MasterMsg::WriteOnce { func, items })
            }
            Cmd::MasterAutoWrite {
                func,
                items,
                interval_ms,
            } => to_active!(MasterMsg::AutoWrite {
                func,
                items,
                interval_ms
            }),
            Cmd::MasterAutoWriteStop => to_active!(MasterMsg::AutoWriteStop),
            Cmd::MasterMaskWrite {
                address,
                and_mask,
                or_mask,
            } => to_active!(MasterMsg::MaskWrite {
                address,
                and_mask,
                or_mask
            }),
            Cmd::MasterReadWrite {
                read_addr,
                read_qty,
                write_addr,
                write_values,
            } => to_active!(MasterMsg::ReadWrite {
                read_addr,
                read_qty,
                write_addr,
                write_values
            }),
            Cmd::MasterFormat(f) => to_active!(MasterMsg::SetFormat(f)),
            Cmd::MasterName { address, name } => to_active!(MasterMsg::SetName { address, name }),
            Cmd::MasterScaling(s) => to_active!(MasterMsg::SetScaling(s)),
            Cmd::MasterColors(c) => to_active!(MasterMsg::SetColors(c)),
            Cmd::MasterValueNames(v) => to_active!(MasterMsg::SetValueNames(v)),
            Cmd::MasterCellFormat { address, format } => {
                to_active!(MasterMsg::SetCellFormat { address, format })
            }
            Cmd::MasterDerived(d) => to_active!(MasterMsg::SetDerived(d)),
            Cmd::MasterChartExport(p) => to_active!(MasterMsg::ExportChart(p)),
            Cmd::MasterChartAxis { addr, right } => {
                to_active!(MasterMsg::SetChartAxis { addr, right })
            }
            Cmd::MasterChartPage(direction) => {
                to_active!(MasterMsg::SetChartPage(direction))
            }
            Cmd::MasterChartFocus(addr) => {
                to_active!(MasterMsg::FocusChartAddress(addr))
            }
            Cmd::MasterReadDef {
                area,
                address,
                quantity,
                scan_ms,
                poll,
            } => {
                to_active!(MasterMsg::SetReadDef {
                    area,
                    address,
                    quantity,
                    scan_ms,
                    poll
                })
            }
            Cmd::MasterStartLog(c) => to_active!(MasterMsg::StartLog(c)),
            Cmd::MasterStopLog => to_active!(MasterMsg::StopLog),

            Cmd::ScanAddress(cfg) => {
                if let Some(s) = scan.take() {
                    s.abort();
                }
                scan = Some(spawn_scan_address(cfg, ui.clone()));
            }
            Cmd::ScanSlave(cfg) => {
                if let Some(s) = scan.take() {
                    s.abort();
                }
                scan = Some(spawn_scan_slave(cfg, ui.clone()));
            }
            Cmd::ScanStop => {
                if let Some(s) = scan.take() {
                    s.abort();
                }
                ui.scan_status("Stopped", false);
            }
            Cmd::SlaveStart(cfg) => {
                if let Some(s) = slave.take() {
                    s.stop();
                }
                slave = Some(spawn_slave(cfg, ui.clone()));
            }
            Cmd::SlaveStop => {
                if let Some(s) = slave.take() {
                    s.stop();
                }
                ui.slave_status("Stopped", false);
            }
            Cmd::SlaveEdit { address, text } => {
                if let Some(s) = &slave {
                    s.send(SlaveMsg::Edit { address, text });
                }
            }
            Cmd::SlaveEditAt {
                area,
                address,
                text,
            } => {
                if let Some(s) = &slave {
                    s.send(SlaveMsg::EditAt {
                        area,
                        address,
                        text,
                    });
                }
            }
            Cmd::SlaveName { address, name } => {
                if let Some(s) = &slave {
                    s.send(SlaveMsg::SetName { address, name });
                }
            }
            Cmd::SlaveCellFormat { address, format } => {
                if let Some(s) = &slave {
                    s.send(SlaveMsg::SetCellFormat { address, format });
                }
            }
            Cmd::ExportCsv {
                path,
                server,
                active_id,
                active_csv,
            } => {
                if server {
                    // 服务端: 交给 updater(它有 names + store 访问)扫 4 表导出
                    if let Some(s) = &slave {
                        s.send(SlaveMsg::ExportCsv(path));
                    }
                } else {
                    // 客户端: 活动窗口用 UI 传来的当前行(名字/功能码正确), 其余窗口从 cache 取。
                    let mut out = String::from("Import,Function,Address,Name,Value\n");
                    out.push_str(&active_csv);
                    for w in windows.iter().filter(|w| w.id != active_id) {
                        let area = func_area_be(w.cfg.function);
                        let c = w.cache.lock().unwrap();
                        for (i, r) in c.rows.iter().enumerate() {
                            let name = c
                                .names
                                .get(i)
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| r.name.to_string());
                            out.push_str(&format!(
                                "yes,{},{},{},{}\n",
                                area.csv_name(),
                                r.address,
                                csv_field(&name),
                                csv_field(&r.value)
                            ));
                        }
                    }
                    let result_ui = ui.clone();
                    tokio::task::spawn_blocking(move || {
                        let result = std::fs::write(&path, out);
                        result_ui.file_result(false, "CSV 导出", path, result);
                    });
                }
            }
            Cmd::SlaveView {
                area,
                address,
                quantity,
                format,
            } => {
                if let Some(s) = &slave {
                    s.send(SlaveMsg::SetView {
                        area,
                        address,
                        quantity,
                        format,
                    });
                }
            }
            Cmd::SlaveScaling(v) => {
                if let Some(s) = &slave {
                    s.send(SlaveMsg::SetScaling(v));
                }
            }
            Cmd::SlaveColors(v) => {
                if let Some(s) = &slave {
                    s.send(SlaveMsg::SetColors(v));
                }
            }
            Cmd::SlaveValueNames(v) => {
                if let Some(s) = &slave {
                    s.send(SlaveMsg::SetValueNames(v));
                }
            }
            Cmd::SlaveSimStart(v) => {
                if let Some(s) = &slave {
                    s.send(SlaveMsg::SimStart(v));
                }
            }
            Cmd::SlaveSimStop => {
                if let Some(s) = &slave {
                    s.send(SlaveMsg::SimStop);
                }
            }
            Cmd::SlaveAutoInc { address } => {
                if let Some(s) = &slave {
                    s.send(SlaveMsg::ToggleAutoInc { address });
                }
            }
        }
    }
}

// ===========================================================================
// Raw-byte tap — captures the exact ADU bytes for the traffic monitor.
// ===========================================================================

#[derive(Debug, Default)]
struct TrafficHealth {
    dropped_chunks: AtomicU64,
    dropped_bytes: AtomicU64,
    high_watermark: AtomicUsize,
}

#[derive(Clone, Debug)]
struct TrafficTx {
    tx: mpsc::Sender<(bool, Vec<u8>)>,
    health: Arc<TrafficHealth>,
}

impl TrafficTx {
    fn send(&self, item: (bool, Vec<u8>)) -> Result<(), ()> {
        let bytes = item.1.len() as u64;
        match self.tx.try_send(item) {
            Ok(()) => {
                self.health.high_watermark.fetch_max(
                    self.tx.max_capacity().saturating_sub(self.tx.capacity()),
                    Ordering::Relaxed,
                );
                Ok(())
            }
            Err(_) => {
                self.health.dropped_chunks.fetch_add(1, Ordering::Relaxed);
                self.health
                    .dropped_bytes
                    .fetch_add(bytes, Ordering::Relaxed);
                Err(())
            }
        }
    }
}

struct TrafficRx {
    rx: mpsc::Receiver<(bool, Vec<u8>)>,
    health: Arc<TrafficHealth>,
}

fn traffic_channel() -> (TrafficTx, TrafficRx) {
    let (tx, rx) = mpsc::channel(TRAFFIC_QUEUE_CAPACITY);
    let health = Arc::new(TrafficHealth::default());
    (
        TrafficTx {
            tx,
            health: health.clone(),
        },
        TrafficRx { rx, health },
    )
}

#[derive(Debug)]
struct Tap<T> {
    inner: T,
    tx: TrafficTx,
}

impl<T: AsyncRead + Unpin> AsyncRead for Tap<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let pre = buf.filled().len();
        let r = Pin::new(&mut this.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &r {
            let post = buf.filled().len();
            if post > pre {
                let _ = this.tx.send((false, buf.filled()[pre..post].to_vec()));
            }
        }
        r
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Tap<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let r = Pin::new(&mut this.inner).poll_write(cx, data);
        if let Poll::Ready(Ok(n)) = &r {
            if *n > 0 {
                let _ = this.tx.send((true, data[..*n].to_vec()));
            }
        }
        r
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

/// 把一个已 connect 的 UDP socket 包装成 AsyncRead+AsyncWrite 流，
/// 使 tokio-modbus 的 MBAP 帧编解码(tcp::attach_slave)能跑在 UDP 之上 = Modbus/UDP。
/// 语义: 一次 poll_write = 发一个请求数据报; 一次 poll_read = 收一个响应数据报。
/// Modbus 的请求/响应天然一来一回、每个 ADU 一个数据报，与 MBAP 长度字段一致。
#[derive(Debug)]
struct UdpStream {
    sock: tokio::net::UdpSocket,
}

impl AsyncRead for UdpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.get_mut().sock.poll_recv(cx, buf)
    }
}

impl AsyncWrite for UdpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.get_mut().sock.poll_send(cx, data)
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// 绑定本地任意端口并 connect 到目标，返回可做 MBAP 帧的 UDP 流。
async fn udp_connect(host: &str, port: u16) -> anyhow::Result<UdpStream> {
    let addr = resolve(host, port)?;
    let sock = tokio::net::UdpSocket::bind(("0.0.0.0", 0)).await?;
    sock.connect(addr).await?;
    Ok(UdpStream { sock })
}

async fn connect_tapped(
    t: &Transport,
    slave_id: u8,
    tls: Option<&crate::tls::TlsClientCfg>,
) -> anyhow::Result<(Context, TrafficRx, Option<String>)> {
    let (tap_tx, tap_rx) = traffic_channel();
    let mut tls_desc = None;
    let ctx = match t {
        Transport::Tcp { host, port } => {
            let addr = resolve(host, *port)?;
            let stream = tokio::net::TcpStream::connect(addr).await?;
            match tls {
                Some(tcfg) => {
                    let connector = crate::tls::client_connector(tcfg)?;
                    let name = crate::tls::server_name(tcfg, host)?;
                    let tls_stream = connector.connect(name, stream).await?;
                    tls_desc = Some(crate::tls::describe(tls_stream.get_ref().1));
                    tcp::attach_slave(
                        Tap {
                            inner: tls_stream,
                            tx: tap_tx,
                        },
                        Slave(slave_id),
                    )
                }
                None => tcp::attach_slave(
                    Tap {
                        inner: stream,
                        tx: tap_tx,
                    },
                    Slave(slave_id),
                ),
            }
        }
        Transport::Udp { host, port } => {
            // Modbus/UDP 用 MBAP 帧(同 TCP)，故仍走 tcp::attach_slave，只是底层是 UDP 流。
            let udp = udp_connect(host, *port).await?;
            tcp::attach_slave(
                Tap {
                    inner: udp,
                    tx: tap_tx,
                },
                Slave(slave_id),
            )
        }
        Transport::RtuOverTcp { host, port } => {
            // RTU 帧(CRC)走 TCP：用 rtu::attach_slave 做 RTU 编解码，底层是 TCP 流。
            let addr = resolve(host, *port)?;
            let stream = tokio::net::TcpStream::connect(addr).await?;
            rtu::attach_slave(
                Tap {
                    inner: stream,
                    tx: tap_tx,
                },
                Slave(slave_id),
            )
        }
        Transport::RtuOverUdp { host, port } => {
            let udp = udp_connect(host, *port).await?;
            rtu::attach_slave(
                Tap {
                    inner: udp,
                    tx: tap_tx,
                },
                Slave(slave_id),
            )
        }
        Transport::Rtu {
            path,
            baud,
            data_bits,
            parity,
            stop_bits,
        } => {
            let serial = SerialStream::open(&serial_builder(
                path, *baud, *data_bits, *parity, *stop_bits,
            ))?;
            rtu::attach_slave(
                Tap {
                    inner: serial,
                    tx: tap_tx,
                },
                Slave(slave_id),
            )
        }
    };
    Ok((ctx, tap_rx, tls_desc))
}

async fn connect_plain(t: &Transport, slave_id: u8) -> anyhow::Result<Context> {
    match t {
        Transport::Tcp { host, port } => {
            let addr = resolve(host, *port)?;
            Ok(tcp::connect_slave(addr, Slave(slave_id)).await?)
        }
        Transport::Udp { host, port } => {
            let udp = udp_connect(host, *port).await?;
            Ok(tcp::attach_slave(udp, Slave(slave_id)))
        }
        Transport::RtuOverTcp { host, port } => {
            let addr = resolve(host, *port)?;
            let stream = tokio::net::TcpStream::connect(addr).await?;
            Ok(rtu::attach_slave(stream, Slave(slave_id)))
        }
        Transport::RtuOverUdp { host, port } => {
            let udp = udp_connect(host, *port).await?;
            Ok(rtu::attach_slave(udp, Slave(slave_id)))
        }
        Transport::Rtu {
            path,
            baud,
            data_bits,
            parity,
            stop_bits,
        } => {
            let serial = SerialStream::open(&serial_builder(
                path, *baud, *data_bits, *parity, *stop_bits,
            ))?;
            Ok(rtu::attach_slave(serial, Slave(slave_id)))
        }
    }
}

fn resolve(host: &str, port: u16) -> anyhow::Result<std::net::SocketAddr> {
    (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("cannot resolve {host}:{port}"))
}

fn fc_name(fc: u8) -> &'static str {
    match fc {
        1 => "Read Coils",
        2 => "Read Discrete Inputs",
        3 => "Read Holding Regs",
        4 => "Read Input Regs",
        5 => "Write Single Coil",
        6 => "Write Single Reg",
        7 => "Read Exception Status",
        8 => "Diagnostics",
        11 => "Get Comm Event Counter",
        15 => "Write Multiple Coils",
        16 => "Write Multiple Regs",
        17 => "Report Server ID",
        22 => "Mask Write Reg",
        23 => "Read/Write Multiple Regs",
        43 => "Encapsulated (MEI)",
        _ => "?",
    }
}
fn modbus_exc_name(e: u8) -> &'static str {
    match e {
        1 => "Illegal Function",
        2 => "Illegal Data Address",
        3 => "Illegal Data Value",
        4 => "Server Device Failure",
        5 => "Acknowledge",
        6 => "Server Busy",
        8 => "Memory Parity Error",
        10 => "Gateway Path Unavailable",
        11 => "Gateway Target Failed to Respond",
        _ => "?",
    }
}
fn u16be(b: &[u8], i: usize) -> u16 {
    u16::from_be_bytes([
        b.get(i).copied().unwrap_or(0),
        b.get(i + 1).copied().unwrap_or(0),
    ])
}

/// 解析 Modbus ADU → 一行人类可读摘要(TID/Unit/Function/地址/数量/字节数/异常码)。
/// bytes = 完整 ADU(TCP 含 MBAP 头, RTU 含 unit+CRC)。无法解析返回 None。
fn parse_modbus_adu(is_tx: bool, bytes: &[u8], is_tcp: bool) -> Option<String> {
    let (tid, unit, pdu): (Option<u16>, u8, &[u8]) = if is_tcp {
        if bytes.len() < 8 {
            return None;
        }
        let body_len = u16be(bytes, 4) as usize;
        if u16be(bytes, 2) != 0 || !(2..=254).contains(&body_len) || bytes.len() != 6 + body_len {
            return None;
        }
        (Some(u16be(bytes, 0)), bytes[6], &bytes[7..])
    } else {
        if bytes.len() < 4 {
            return None;
        } // unit + fc + ... + CRC(2)
        (None, bytes[0], &bytes[1..bytes.len() - 2])
    };
    if pdu.is_empty() {
        return None;
    }
    let fc = pdu[0];
    let mut s = String::new();
    if let Some(t) = tid {
        s += &format!("TID={t} ");
    }
    s += &format!("U={unit} ");
    if fc & 0x80 != 0 {
        let e = pdu.get(1).copied().unwrap_or(0);
        s += &format!(
            "FC{:02} ⚠ Exception {:02X} ({})",
            fc & 0x7F,
            e,
            modbus_exc_name(e)
        );
        return Some(s);
    }
    s += &format!("FC{fc:02} {}", fc_name(fc));
    let p = &pdu[1..]; // fc 之后的参数
    match fc {
        1..=4 => {
            if is_tx {
                s += &format!(" addr={} qty={}", u16be(p, 0), u16be(p, 2));
            } else {
                s += &format!(" bytes={}", p.first().copied().unwrap_or(0));
            }
        }
        5 | 6 => {
            s += &format!(" addr={} val=0x{:04X}", u16be(p, 0), u16be(p, 2));
        }
        15 | 16 => {
            s += &format!(" addr={} qty={}", u16be(p, 0), u16be(p, 2));
        }
        22 => {
            s += &format!(
                " addr={} and=0x{:04X} or=0x{:04X}",
                u16be(p, 0),
                u16be(p, 2),
                u16be(p, 4)
            );
        }
        23 => {
            if is_tx {
                s += &format!(
                    " rd_addr={} rd_qty={} wr_addr={} wr_qty={}",
                    u16be(p, 0),
                    u16be(p, 2),
                    u16be(p, 4),
                    u16be(p, 6)
                );
            } else {
                s += &format!(" bytes={}", p.first().copied().unwrap_or(0));
            }
        }
        _ => {}
    }
    Some(s)
}

const MAX_MBAP_ADU: usize = 260;

#[derive(Default)]
struct MbapReassembler {
    bytes: Vec<u8>,
}

impl MbapReassembler {
    fn push(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.bytes.extend_from_slice(chunk);
        let mut frames = Vec::new();
        loop {
            if self.bytes.len() < 7 {
                break;
            }
            let protocol = u16::from_be_bytes([self.bytes[2], self.bytes[3]]);
            let body_len = u16::from_be_bytes([self.bytes[4], self.bytes[5]]) as usize;
            let total = 6usize.saturating_add(body_len);
            if protocol != 0 || !(2..=254).contains(&body_len) || total > MAX_MBAP_ADU {
                self.bytes.remove(0);
                continue;
            }
            if self.bytes.len() < total {
                break;
            }
            frames.push(self.bytes.drain(..total).collect());
        }
        frames
    }
}

fn spawn_traffic_forwarder(mut rx: TrafficRx, sink: WindowSink, is_tcp: bool) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut buf: Vec<crate::LogLine> = Vec::new();
        let mut tx_frames = MbapReassembler::default();
        let mut rx_frames = MbapReassembler::default();
        let mut health_tick = tokio::time::interval(Duration::from_secs(1));
        let mut flush_tick = tokio::time::interval(Duration::from_millis(50));
        let mut reported_drops = 0;
        let mut dirty = false;
        loop {
            tokio::select! {
                _ = flush_tick.tick(), if dirty => {
                    sink.traffic(buf.clone());
                    dirty = false;
                }
                _ = health_tick.tick() => {
                    let dropped = rx.health.dropped_chunks.load(Ordering::Relaxed);
                    if dropped > reported_drops {
                        let bytes = rx.health.dropped_bytes.load(Ordering::Relaxed);
                        let high = rx.health.high_watermark.load(Ordering::Relaxed);
                        buf.push(crate::LogLine {
                            time: now_hms().into(),
                            dir: "ERR".into(),
                            text: format!(
                                "Traffic monitor queue overflow: dropped {dropped} chunks / {bytes} bytes, H{high}/{TRAFFIC_QUEUE_CAPACITY}"
                            ).into(),
                        });
                        if buf.len() > 500 {
                            let excess = buf.len() - 500;
                            buf.drain(0..excess);
                        }
                        dirty = true;
                        reported_drops = dropped;
                    }
                }
                item = rx.rx.recv() => {
                    let Some((is_tx, chunk)) = item else { break };
                    let frames = if is_tcp {
                        if is_tx {
                            tx_frames.push(&chunk)
                        } else {
                            rx_frames.push(&chunk)
                        }
                    } else {
                        vec![chunk]
                    };
                    for bytes in frames {
                        let hex = bytes
                            .iter()
                            .map(|b| format!("{b:02X}"))
                            .collect::<Vec<_>>()
                            .join(" ");
                        let text = match parse_modbus_adu(is_tx, &bytes, is_tcp) {
                            Some(p) => format!("{p}   |   {hex}"),
                            None => hex,
                        };
                        buf.push(crate::LogLine {
                            time: now_hms().into(),
                            dir: if is_tx { "Tx" } else { "Rx" }.into(),
                            text: text.into(),
                        });
                        if buf.len() > 500 {
                            let excess = buf.len() - 500;
                            buf.drain(0..excess);
                        }
                        dirty = true;
                    }
                }
            }
        }
        if dirty {
            sink.traffic(buf);
        }
    })
}

// ===========================================================================
// Master (poll) engine
// ===========================================================================

mod master;
use master::*;

// ===========================================================================
// Scan engine
// ===========================================================================

enum ScanErr {
    Exception(String),
    Timeout,
    Io(String),
}

async fn probe(ctx: &mut Context, area: Area, addr: u16) -> Result<String, ScanErr> {
    let dur = Duration::from_millis(SCAN_TIMEOUT_MS);
    match area {
        Area::Coils => match tokio::time::timeout(dur, ctx.read_coils(addr, 1)).await {
            Ok(Ok(Ok(v))) => Ok(format!(
                "coil = {}",
                v.first().map(|b| *b as u8).unwrap_or(0)
            )),
            Ok(Ok(Err(e))) => Err(ScanErr::Exception(format!("{e:?}"))),
            Ok(Err(e)) => Err(ScanErr::Io(e.to_string())),
            Err(_) => Err(ScanErr::Timeout),
        },
        Area::DiscreteInputs => {
            match tokio::time::timeout(dur, ctx.read_discrete_inputs(addr, 1)).await {
                Ok(Ok(Ok(v))) => Ok(format!(
                    "input = {}",
                    v.first().map(|b| *b as u8).unwrap_or(0)
                )),
                Ok(Ok(Err(e))) => Err(ScanErr::Exception(format!("{e:?}"))),
                Ok(Err(e)) => Err(ScanErr::Io(e.to_string())),
                Err(_) => Err(ScanErr::Timeout),
            }
        }
        Area::HoldingRegisters => {
            match tokio::time::timeout(dur, ctx.read_holding_registers(addr, 1)).await {
                Ok(Ok(Ok(v))) => Ok(format!("0x{0:04X} ({0})", v.first().copied().unwrap_or(0))),
                Ok(Ok(Err(e))) => Err(ScanErr::Exception(format!("{e:?}"))),
                Ok(Err(e)) => Err(ScanErr::Io(e.to_string())),
                Err(_) => Err(ScanErr::Timeout),
            }
        }
        Area::InputRegisters => {
            match tokio::time::timeout(dur, ctx.read_input_registers(addr, 1)).await {
                Ok(Ok(Ok(v))) => Ok(format!("0x{0:04X} ({0})", v.first().copied().unwrap_or(0))),
                Ok(Ok(Err(e))) => Err(ScanErr::Exception(format!("{e:?}"))),
                Ok(Err(e)) => Err(ScanErr::Io(e.to_string())),
                Err(_) => Err(ScanErr::Timeout),
            }
        }
    }
}

fn scan_row(addr_or_id: String, res: &Result<String, ScanErr>) -> crate::LogLine {
    let (dir, text) = match res {
        Ok(s) => ("OK", format!("{addr_or_id}: {s}")),
        Err(ScanErr::Exception(e)) => ("ERR", format!("{addr_or_id}: exception {e}")),
        Err(ScanErr::Timeout) => ("…", format!("{addr_or_id}: no response")),
        Err(ScanErr::Io(e)) => ("ERR", format!("{addr_or_id}: {e}")),
    };
    crate::LogLine {
        time: now_hms().into(),
        dir: dir.into(),
        text: text.into(),
    }
}

fn spawn_scan_address(cfg: ScanCfg, ui: UiSink) -> JoinHandle<()> {
    tokio::spawn(async move {
        ui.scan_rows(Vec::new());
        ui.scan_status("Connecting for address scan …", true);
        let mut ctx = match connect_plain(&cfg.transport, cfg.slave_id).await {
            Ok(c) => c,
            Err(e) => {
                ui.scan_status(format!("Connect failed: {e}"), false);
                return;
            }
        };
        let mut rows: Vec<crate::LogLine> = Vec::new();
        let mut found = 0u32;
        for off in 0..cfg.count {
            // 不跨 16 位地址空间回绕重扫低地址：start+off 超过 0xFFFF 即停。
            let addr32 = cfg.start as u32 + off as u32;
            if addr32 > 0xFFFF {
                break;
            }
            let addr = addr32 as u16;
            let res = probe(&mut ctx, cfg.area, addr).await;
            if res.is_ok() {
                found += 1;
            }
            rows.push(scan_row(format!("@{addr}"), &res));
            ui.scan_rows(rows.clone());
            ui.scan_status(
                format!(
                    "Address scan {}/{} — {} responded",
                    off + 1,
                    cfg.count,
                    found
                ),
                true,
            );
        }
        let _ = ctx.disconnect().await;
        ui.scan_status(
            format!("Address scan done — {} of {} responded", found, cfg.count),
            false,
        );
    })
}

fn spawn_scan_slave(cfg: SlaveScanCfg, ui: UiSink) -> JoinHandle<()> {
    tokio::spawn(async move {
        ui.scan_rows(Vec::new());
        ui.scan_status("Connecting for slave scan …", true);
        let mut ctx = match connect_plain(&cfg.transport, cfg.from_id).await {
            Ok(c) => c,
            Err(e) => {
                ui.scan_status(format!("Connect failed: {e}"), false);
                return;
            }
        };
        let mut rows: Vec<crate::LogLine> = Vec::new();
        let mut found = 0u32;
        let (lo, hi) = (cfg.from_id.min(cfg.to_id), cfg.from_id.max(cfg.to_id));
        for id in lo..=hi {
            ctx.set_slave(Slave(id));
            let res = probe(&mut ctx, cfg.area, cfg.address).await;
            if res.is_ok() {
                found += 1;
            }
            rows.push(scan_row(format!("ID {id}"), &res));
            ui.scan_rows(rows.clone());
            ui.scan_status(format!("Slave scan {lo}…{hi} — {found} responded"), true);
        }
        let _ = ctx.disconnect().await;
        ui.scan_status(
            format!("Slave scan done — {found} slave(s) responded"),
            false,
        );
    })
}

// ===========================================================================
// Slave (simulator) engine
// ===========================================================================

mod slave;
use slave::*;

fn serial_builder(
    path: &str,
    baud: u32,
    data_bits: u8,
    parity: u8,
    stop_bits: u8,
) -> tokio_serial::SerialPortBuilder {
    use tokio_serial::{DataBits, Parity, StopBits};
    tokio_serial::new(path, baud)
        .data_bits(match data_bits {
            5 => DataBits::Five,
            6 => DataBits::Six,
            7 => DataBits::Seven,
            _ => DataBits::Eight,
        })
        .parity(match parity {
            1 => Parity::Even,
            2 => Parity::Odd,
            _ => Parity::None,
        })
        .stop_bits(match stop_bits {
            2 => StopBits::Two,
            _ => StopBits::One,
        })
}

// ===========================================================================
// UI bridge
// ===========================================================================

mod ui_bridge;
use ui_bridge::*;

mod grid;
use grid::*;

fn push_log(buf: &mut Vec<crate::LogLine>, dir: &str, text: String) -> Vec<crate::LogLine> {
    buf.push(crate::LogLine {
        time: now_hms().into(),
        dir: dir.into(),
        text: text.into(),
    });
    if buf.len() > 300 {
        let excess = buf.len() - 300;
        buf.drain(0..excess);
    }
    buf.clone()
}

fn now_hms() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    format!("{h:02}:{m:02}:{s:02}")
}

/// UTC "YYYY-MM-DD HH:MM:SS" timestamp for log files (civil date via Hinnant's algorithm).
fn now_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    if mth <= 2 {
        y += 1;
    }
    format!("{y:04}-{mth:02}-{d:02} {hh:02}:{mm:02}:{ss:02}")
}

// ===========================================================================
// Tests — exercise the real Modbus master/slave stack end to end (no GUI).
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_error_explains_unavailable_windows_address_in_both_languages() {
        let error = std::io::Error::from_raw_os_error(10049);
        let zh = bind_error_message("192.168.1.110", 502, &error, false);
        let en = bind_error_message("192.168.1.110", 502, &error, true);
        assert!(zh.contains("当前不能用于监听"));
        assert!(zh.contains("网卡可能未连接"));
        assert!(zh.contains("IP 地址尚未生效"));
        assert!(zh.contains("0.0.0.0"));
        assert!(en.contains("not currently available for listening"));
        assert!(en.contains("network adapter may be disconnected"));
        assert!(en.contains("IP address may not be active yet"));
        assert!(en.contains("0.0.0.0"));
    }

    #[test]
    fn bind_error_explains_port_conflict() {
        let error = std::io::Error::new(std::io::ErrorKind::AddrInUse, "in use");
        assert!(bind_error_message("0.0.0.0", 502, &error, false).contains("端口 502"));
        assert!(bind_error_message("0.0.0.0", 502, &error, true).contains("port 502"));
    }

    struct TestCertificates {
        _directory: tempfile::TempDir,
        ca_cert: String,
        server_cert: String,
        server_key: String,
        client_cert: String,
        client_key: String,
    }

    fn test_certificates() -> TestCertificates {
        use rcgen::{
            BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
            KeyPair, KeyUsagePurpose,
        };

        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "PCAN-Explorer10 ephemeral test CA");
        ca_params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        let ca_key = KeyPair::generate().unwrap();
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca_params, ca_key);

        let mut server_params = CertificateParams::new(vec!["localhost".into()]).unwrap();
        server_params
            .distinguished_name
            .push(DnType::CommonName, "localhost");
        server_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let server_key = KeyPair::generate().unwrap();
        let server_cert = server_params.signed_by(&server_key, &issuer).unwrap();

        let mut client_params =
            CertificateParams::new(vec!["pcanwork-test-client".into()]).unwrap();
        client_params
            .distinguished_name
            .push(DnType::CommonName, "pcanwork-test-client");
        client_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let client_key = KeyPair::generate().unwrap();
        let client_cert = client_params.signed_by(&client_key, &issuer).unwrap();

        let directory = tempfile::tempdir().unwrap();
        let ca_cert_path = directory.path().join("ca.crt");
        let server_cert_path = directory.path().join("server.crt");
        let server_key_path = directory.path().join("server.key");
        let client_cert_path = directory.path().join("client.crt");
        let client_key_path = directory.path().join("client.key");
        std::fs::write(&ca_cert_path, ca_cert.pem()).unwrap();
        std::fs::write(&server_cert_path, server_cert.pem()).unwrap();
        std::fs::write(&server_key_path, server_key.serialize_pem()).unwrap();
        std::fs::write(&client_cert_path, client_cert.pem()).unwrap();
        std::fs::write(&client_key_path, client_key.serialize_pem()).unwrap();

        TestCertificates {
            ca_cert: ca_cert_path.to_string_lossy().into_owned(),
            server_cert: server_cert_path.to_string_lossy().into_owned(),
            server_key: server_key_path.to_string_lossy().into_owned(),
            client_cert: client_cert_path.to_string_lossy().into_owned(),
            client_key: client_key_path.to_string_lossy().into_owned(),
            _directory: directory,
        }
    }

    #[test]
    fn protocol_spans_enforce_modbus_limits_and_address_space() {
        assert!(validate_read_definition(Area::Coils, 0, 2000).is_ok());
        assert!(validate_read_definition(Area::Coils, 0, 2001).is_err());
        assert!(validate_read_definition(Area::HoldingRegisters, 0, 125).is_ok());
        assert!(validate_read_definition(Area::HoldingRegisters, 0, 126).is_err());
        assert!(validate_read_definition(Area::HoldingRegisters, 65_535, 2).is_err());
        assert!(validate_span(0, 1968, 1968, "FC15").is_ok());
        assert!(validate_span(0, 1969, 1968, "FC15").is_err());
        assert!(validate_span(0, 123, 123, "FC16").is_ok());
        assert!(validate_span(0, 124, 123, "FC16").is_err());
    }

    #[test]
    fn mbap_reassembler_handles_split_and_sticky_frames() {
        let first = [
            0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x01, 0x03, 0x00, 0x00, 0x00, 0x01,
        ];
        let second = [
            0x00, 0x02, 0x00, 0x00, 0x00, 0x06, 0x01, 0x03, 0x00, 0x01, 0x00, 0x01,
        ];

        let mut split = MbapReassembler::default();
        assert!(split.push(&first[..5]).is_empty());
        assert_eq!(split.push(&first[5..]), vec![first.to_vec()]);

        let mut sticky = MbapReassembler::default();
        let mut joined = first.to_vec();
        joined.extend_from_slice(&second);
        assert_eq!(sticky.push(&joined), vec![first.to_vec(), second.to_vec()]);
    }

    #[test]
    fn multiple_write_rejects_non_contiguous_addresses() {
        let items = [
            WriteItem {
                address: 100,
                value: 1,
                inc: false,
            },
            WriteItem {
                address: 102,
                value: 3,
                inc: false,
            },
        ];
        assert!(validate_write_items(WriteFunc::MultiRegs, &items).is_err());
        assert!(validate_write_items(WriteFunc::SingleReg, &items).is_ok());
    }

    #[tokio::test]
    async fn udp_master_round_trip() {
        use tokio::net::UdpSocket;
        // 最小 UDP Modbus(MBAP)服务器: 对任意请求回 Read Coils 响应(coil 0,2,9 置位)。
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = server.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut buf = [0u8; 260];
            let (_n, peer) = server.recv_from(&mut buf).await.unwrap();
            let (tid_hi, tid_lo, unit) = (buf[0], buf[1], buf[6]); // 回显 TID/unit
                                                                   // MBAP: tid, proto=0, len=5 | PDU: func=01, bytecount=2, data=0x05,0x02
            let resp = [
                tid_hi, tid_lo, 0x00, 0x00, 0x00, 0x05, unit, 0x01, 0x02, 0x05, 0x02,
            ];
            server.send_to(&resp, peer).await.unwrap();
        });
        // 用新增的 Modbus/UDP 传输连过去读 10 个线圈
        let mut ctx = connect_plain(
            &Transport::Udp {
                host: "127.0.0.1".into(),
                port,
            },
            1,
        )
        .await
        .expect("UDP connect");
        let coils = ctx.read_coils(0, 10).await.unwrap().unwrap();
        assert_eq!(coils.len(), 10);
        assert_eq!(
            coils,
            vec![true, false, true, false, false, false, false, false, false, true]
        );
    }

    #[tokio::test]
    async fn rtu_over_tcp_round_trip() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // 标准 Modbus CRC16
        fn crc16(data: &[u8]) -> u16 {
            let mut crc: u16 = 0xFFFF;
            for &b in data {
                crc ^= b as u16;
                for _ in 0..8 {
                    if crc & 1 != 0 {
                        crc = (crc >> 1) ^ 0xA001
                    } else {
                        crc >>= 1
                    }
                }
            }
            crc
        }
        // 手写 RTU-over-TCP 服务端: 收到请求后回 Read Holding Registers(1 个寄存器=0x1092)。
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _peer) = listener.accept().await.unwrap();
            let mut buf = [0u8; 256];
            let _ = stream.read(&mut buf).await.unwrap(); // 收请求(RTU 帧)
            let mut resp = vec![0x01u8, 0x03, 0x02, 0x10, 0x92]; // unit,func,bytecount,data
            let c = crc16(&resp);
            resp.push((c & 0xff) as u8);
            resp.push((c >> 8) as u8);
            stream.write_all(&resp).await.unwrap();
        });
        let mut ctx = connect_plain(
            &Transport::RtuOverTcp {
                host: "127.0.0.1".into(),
                port: addr.port(),
            },
            1,
        )
        .await
        .unwrap();
        let h = ctx.read_holding_registers(0, 1).await.unwrap().unwrap();
        assert_eq!(h, vec![0x1092]);
    }

    #[test]
    fn server_csv_export() {
        let mut store = DataStore::new();
        store.holding[0] = 28;
        store.holding[5] = 1000;
        store.coils[2] = true;
        let mut names: HashMap<u16, String> = HashMap::new();
        names.insert(0, "Temp".into());
        let csv = build_server_csv(&store, &names);
        assert_eq!(
            csv.lines().next().unwrap(),
            "Import,Function,Address,Name,Value"
        );
        assert!(csv.contains("yes,Coils,2,,1"), "{csv}"); // 线圈 2 = ON
        assert!(csv.contains("yes,HoldingRegisters,0,Temp,28"), "{csv}"); // 命名 + 值
        assert!(csv.contains("yes,HoldingRegisters,5,,1000"), "{csv}"); // 仅非零值
                                                                        // 命名地址 0 全局 → 各表都出一行(含值为 0 的); 但绝不导出 26 万空寄存器
        assert!(
            csv.lines().count() < 50,
            "filtered empties, got {}",
            csv.lines().count()
        );
        assert!(
            !csv.contains("yes,HoldingRegisters,6,"),
            "空寄存器不应导出\n{csv}"
        );
    }

    #[test]
    fn csv_address_lenient() {
        assert_eq!(
            Area::from_csv_name("HoldingRegisters"),
            Some(Area::HoldingRegisters)
        );
        assert_eq!(Area::from_csv_name("coils"), Some(Area::Coils));
        assert_eq!(Area::from_csv_name("4x"), Some(Area::HoldingRegisters));
        assert_eq!(Area::from_csv_name("9"), None);
    }

    #[test]
    fn modbus_adu_parsing() {
        // TCP 读保持寄存器请求: MBAP(TID=1,proto=0,len=6,unit=1) + PDU(03, addr=0, qty=10)
        let req = [
            0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x01, 0x03, 0x00, 0x00, 0x00, 0x0A,
        ];
        let s = parse_modbus_adu(true, &req, true).unwrap();
        assert!(s.contains("TID=1") && s.contains("U=1") && s.contains("FC03"));
        assert!(s.contains("addr=0") && s.contains("qty=10"), "{s}");
        // TCP 响应: 字节数=4 + 2 个寄存器
        let resp = [
            0x00, 0x01, 0x00, 0x00, 0x00, 0x07, 0x01, 0x03, 0x04, 0x12, 0x34, 0x56, 0x78,
        ];
        let s = parse_modbus_adu(false, &resp, true).unwrap();
        assert!(s.contains("FC03") && s.contains("bytes=4"), "{s}");
        // 异常响应: FC=0x83, exception=0x02 (Illegal Data Address)
        let exc = [0x00, 0x01, 0x00, 0x00, 0x00, 0x03, 0x01, 0x83, 0x02];
        let s = parse_modbus_adu(false, &exc, true).unwrap();
        assert!(
            s.contains("Exception 02") && s.contains("Illegal Data Address"),
            "{s}"
        );
        // RTU 请求: unit=1 + PDU(03,addr=0,qty=10) + CRC(2)
        let rtu = [0x01, 0x03, 0x00, 0x00, 0x00, 0x0A, 0xC5, 0xCD];
        let s = parse_modbus_adu(true, &rtu, false).unwrap();
        assert!(
            !s.contains("TID") && s.contains("U=1") && s.contains("addr=0") && s.contains("qty=10"),
            "{s}"
        );
        // 太短 → None
        assert!(parse_modbus_adu(true, &[0x00, 0x01], true).is_none());
    }

    fn make_shared() -> Arc<SlaveShared> {
        let (events_tx, rx) = mpsc::channel(1024);
        std::mem::forget(rx);
        Arc::new(SlaveShared {
            store: Mutex::new(DataStore::new()),
            requests: AtomicU64::new(0),
            events: events_tx,
            event_drops: AtomicU64::new(0),
            event_high_watermark: AtomicUsize::new(0),
        })
    }

    #[tokio::test]
    async fn tcp_master_slave_round_trip() {
        let shared = make_shared();
        {
            let mut s = shared.store.lock().unwrap();
            s.input[5] = 0x1234;
            s.holding[10] = 7;
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let svc = SlaveService {
            shared: shared.clone(),
            unit_id: 1,
            tcp: true,
            ignore_unit_id: true,
        };
        let server = tokio::spawn(async move {
            let on_connected = move |stream: tokio::net::TcpStream, peer: std::net::SocketAddr| {
                let svc = svc.clone();
                async move {
                    tokio_modbus::server::tcp::accept_tcp_connection(stream, peer, move |_addr| {
                        Ok(Some(svc.clone()))
                    })
                }
            };
            let on_error = |e: std::io::Error| eprintln!("server error: {e}");
            let srv = tokio_modbus::server::tcp::Server::new(listener);
            let _ = srv.serve(&on_connected, on_error).await;
        });

        let mut ctx = tcp::connect_slave(addr, Slave(1)).await.unwrap();

        let v = ctx.read_input_registers(5, 1).await.unwrap().unwrap();
        assert_eq!(v, vec![0x1234]);

        ctx.write_single_register(10, 999).await.unwrap().unwrap();
        let h = ctx.read_holding_registers(10, 1).await.unwrap().unwrap();
        assert_eq!(h, vec![999]);
        assert_eq!(shared.store.lock().unwrap().holding[10], 999);

        ctx.write_multiple_coils(0, &[true, false, true])
            .await
            .unwrap()
            .unwrap();
        let c = ctx.read_coils(0, 3).await.unwrap().unwrap();
        assert_eq!(c, vec![true, false, true]);

        ctx.write_multiple_registers(20, &[111, 222, 333])
            .await
            .unwrap()
            .unwrap();
        let r = ctx.read_holding_registers(20, 3).await.unwrap().unwrap();
        assert_eq!(r, vec![111, 222, 333]);

        let exc = ctx.read_holding_registers(65535, 5).await.unwrap();
        assert!(exc.is_err());

        // FC23 over the wire: write [7,8] @30 then read them back.
        let rw = ctx
            .read_write_multiple_registers(30, 2, 30, &[7, 8])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rw, vec![7, 8]);
        assert_eq!(shared.store.lock().unwrap().holding[30], 7);
        assert_eq!(shared.store.lock().unwrap().holding[31], 8);

        server.abort();
    }

    async fn udp_slave_round_trip(rtu: bool) {
        let shared = make_shared();
        shared.store.lock().unwrap().holding[12] = 0x1234;
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = socket.local_addr().unwrap().port();
        let service = SlaveService {
            shared: shared.clone(),
            unit_id: 1,
            tcp: !rtu,
            ignore_unit_id: false,
        };
        let (traffic_tx, traffic_rx) = traffic_channel();
        std::mem::forget(traffic_rx);
        let server = tokio::spawn(serve_udp_slave(socket, service, traffic_tx, rtu));
        let transport = if rtu {
            Transport::RtuOverUdp {
                host: "127.0.0.1".into(),
                port,
            }
        } else {
            Transport::Udp {
                host: "127.0.0.1".into(),
                port,
            }
        };
        let mut context = connect_plain(&transport, 1).await.unwrap();
        assert_eq!(
            context
                .read_holding_registers(12, 1)
                .await
                .unwrap()
                .unwrap(),
            vec![0x1234]
        );
        context
            .write_single_register(12, 0x5678)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(shared.store.lock().unwrap().holding[12], 0x5678);
        server.abort();
    }

    #[tokio::test]
    async fn udp_slave_server_round_trip() {
        udp_slave_round_trip(false).await;
    }

    #[tokio::test]
    async fn rtu_over_udp_slave_server_round_trip() {
        udp_slave_round_trip(true).await;
    }

    #[tokio::test]
    async fn rtu_over_tcp_slave_server_round_trip() {
        let shared = make_shared();
        shared.store.lock().unwrap().holding[7] = 0xCAFE;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let service = SlaveService {
            shared,
            unit_id: 1,
            tcp: false,
            ignore_unit_id: false,
        };
        let server = tokio::spawn(async move {
            let on_connected = move |stream: tokio::net::TcpStream, peer: std::net::SocketAddr| {
                let service = service.clone();
                async move {
                    tokio_modbus::server::rtu_over_tcp::accept_tcp_connection(
                        stream,
                        peer,
                        move |_address| Ok(Some(service.clone())),
                    )
                }
            };
            let instance = tokio_modbus::server::rtu_over_tcp::Server::new(listener);
            let _ = instance.serve(&on_connected, |_| {}).await;
        });
        let mut context = connect_plain(
            &Transport::RtuOverTcp {
                host: "127.0.0.1".into(),
                port,
            },
            1,
        )
        .await
        .unwrap();
        assert_eq!(
            context.read_holding_registers(7, 1).await.unwrap().unwrap(),
            vec![0xCAFE]
        );
        server.abort();
    }

    #[test]
    fn handle_request_bounds() {
        let shared = make_shared();
        let r = handle_request(&shared, &Request::ReadHoldingRegisters(65535, 5));
        assert!(matches!(r, Err(ExceptionCode::IllegalDataAddress)));
    }

    #[test]
    fn mask_write_and_read_write_multiple() {
        let shared = make_shared();
        shared.store.lock().unwrap().holding[5] = 0x00F2;
        // FC22: result = (cur & and) | (or & !and) = 0x00F2 | 0xFF00 = 0xFFF2
        let r = handle_request(&shared, &Request::MaskWriteRegister(5, 0x00FF, 0xFF00));
        assert!(r.is_ok());
        assert_eq!(shared.store.lock().unwrap().holding[5], 0xFFF2);

        // FC23: write [10,20,30] @0 then read 3 @0
        let r = handle_request(
            &shared,
            &Request::ReadWriteMultipleRegisters(
                0,
                3,
                0,
                std::borrow::Cow::Owned(vec![10, 20, 30]),
            ),
        );
        match r {
            Ok(Response::ReadWriteMultipleRegisters(v)) => assert_eq!(v, vec![10, 20, 30]),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn sim_increment_wraps() {
        let mut store = DataStore::new();
        store.holding[0] = 8;
        let cfg = SimCfg {
            mode: SimMode::Increment,
            area: Area::HoldingRegisters,
            address: 0,
            quantity: 1,
            step: 5,
            min: 0,
            max: 10,
            interval_ms: 100,
            target: -1,
        };
        let mut rng = Xorshift::new(1);
        apply_sim(&mut store, &cfg, &mut rng); // 8+5=13 > 10 → wrap to min 0
        assert_eq!(store.holding[0], 0);
        apply_sim(&mut store, &cfg, &mut rng); // 0+5=5
        assert_eq!(store.holding[0], 5);
    }

    #[test]
    fn color_rules_eval() {
        let rules = ColorRules {
            normal: 0,
            op1: CmpOp::Gt,
            v1: 100.0,
            c1: 0xFFFF0000,
            op2: CmpOp::Lt,
            v2: 10.0,
            c2: 0xFF00FF00,
        };
        assert_eq!(rules.eval(150.0), 0xFFFF0000);
        assert_eq!(rules.eval(5.0), 0xFF00FF00);
        assert_eq!(rules.eval(50.0), 0);
    }

    #[test]
    fn chart_path_built() {
        let mut ch = ChartState::new();
        for v in [10.0f64, 20.0, 30.0] {
            update_chart(
                &mut ch,
                &[
                    DisplayRow {
                        address: 0,
                        value: String::new(),
                        raw: String::new(),
                        num: Some(v),
                    },
                    DisplayRow {
                        address: 1,
                        value: String::new(),
                        raw: String::new(),
                        num: Some(v * 2.0),
                    },
                ],
            );
        }
        assert_eq!(ch.data.len(), 2);
        assert_eq!(ch.data[0].len(), 3);

        let (charts, has) = build_charts(&ch);
        assert!(has);
        assert_eq!(charts.len(), 2);
        let cmd = charts[0].commands.to_string();
        assert!(cmd.starts_with("M "), "path must start with MoveTo: {cmd}");
        assert!(cmd.contains(" L "), "path must contain LineTo: {cmd}");
        for tok in cmd.split_whitespace() {
            if let Ok(n) = tok.parse::<f64>() {
                assert!(
                    (0.0..=1000.0).contains(&n),
                    "coord {n} out of range in {cmd}"
                );
            }
        }
        // each register is scaled to its OWN range
        assert_eq!(charts[0].ymax.to_string(), "30.00"); // series 0 max = 30
        assert_eq!(charts[0].ymin.to_string(), "10.00");
        assert_eq!(charts[1].ymax.to_string(), "60.00"); // series 1 max = 30*2
    }

    #[test]
    fn write_item_inc_wraps() {
        let mut items = [
            WriteItem {
                address: 0,
                value: 65534,
                inc: true,
            },
            WriteItem {
                address: 1,
                value: 50,
                inc: false,
            },
        ];
        // simulate one auto-write cycle's increment step
        for it in items.iter_mut() {
            if it.inc {
                it.value = it.value.wrapping_add(1);
            }
        }
        assert_eq!(items[0].value, 65535);
        assert_eq!(items[1].value, 50); // non-inc unchanged
        for it in items.iter_mut() {
            if it.inc {
                it.value = it.value.wrapping_add(1);
            }
        }
        assert_eq!(items[0].value, 0); // wrapped
    }

    #[test]
    fn slave_sim_single_register() {
        let mut mem = vec![0u16; 5];
        let cfg = SimCfg {
            mode: SimMode::Increment,
            area: Area::HoldingRegisters,
            address: 1,
            quantity: 4,
            step: 5,
            min: 0,
            max: 100,
            interval_ms: 100,
            target: 3, // only absolute index 3 within [1,5)
        };
        let mut rng = Xorshift::new(3);
        sim_regs(&mut mem, 1, 5, &cfg, &mut rng);
        assert_eq!(mem, vec![0, 0, 0, 5, 0]); // only index 3 moved
    }

    #[test]
    fn overlay_series_dual_axis() {
        let mut ch = ChartState::new();
        for v in [10.0f64, 20.0, 30.0] {
            update_chart(
                &mut ch,
                &[
                    DisplayRow {
                        address: 0,
                        value: String::new(),
                        raw: String::new(),
                        num: Some(v),
                    },
                    DisplayRow {
                        address: 1,
                        value: String::new(),
                        raw: String::new(),
                        num: Some(v * 100.0),
                    },
                ],
            );
        }
        // both on the left axis by default → shared range 10..3000
        let sb = build_series(&ch);
        assert_eq!(sb.series.len(), 2);
        assert_eq!(sb.left_min, "10.00");
        assert_eq!(sb.left_max, "3000.00");
        assert!(!sb.has_right);

        // assign addr 1 to the right axis → each axis ranges independently
        ch.right.insert(1);
        let sb = build_series(&ch);
        assert!(sb.has_right);
        assert_eq!(sb.left_min, "10.00");
        assert_eq!(sb.left_max, "30.00"); // only addr 0
        assert_eq!(sb.right_min, "1000.00");
        assert_eq!(sb.right_max, "3000.00"); // only addr 1
        assert_eq!(sb.series[0].axis, 0);
        assert_eq!(sb.series[1].axis, 1);
        let cmd = sb.series[1].commands.to_string();
        assert!(cmd.starts_with("M "));
        // the right-axis series is normalised to ITS own range, so it spans the
        // full 0..300 y space (last sample = max → y≈0, first = min → y≈300)
        assert!(
            cmd.contains(" 0.0") || cmd.contains(" 0 "),
            "max should map near y=0: {cmd}"
        );
    }

    #[test]
    fn chart_pages_cover_addresses_beyond_the_first_six() {
        let rows: Vec<DisplayRow> = (0..25)
            .map(|address| DisplayRow {
                address,
                value: String::new(),
                raw: String::new(),
                num: Some(address as f64),
            })
            .collect();
        let mut ch = ChartState::new();

        update_chart(&mut ch, &rows);
        update_chart(&mut ch, &rows);
        assert_eq!(ch.total, 25);
        assert_eq!(ch.addrs, (0..12).collect::<Vec<_>>());
        assert_eq!(build_series(&ch).series.len(), 12);

        assert!(ch.move_page(1));
        update_chart(&mut ch, &rows);
        assert_eq!(ch.page_start, 12);
        assert_eq!(ch.addrs, (12..24).collect::<Vec<_>>());

        assert!(ch.move_page(1));
        update_chart(&mut ch, &rows);
        assert_eq!(ch.page_start, 24);
        assert_eq!(ch.addrs, vec![24]);
        assert!(!ch.move_page(1));

        ch.page_start = 0;
        assert!(ch.focus_address(&rows, 19));
        update_chart(&mut ch, &rows);
        assert_eq!(ch.page_start, 12);
        assert!(ch.addrs.contains(&19));
    }

    #[test]
    fn log_line_written() {
        let dir = std::env::temp_dir();
        let path = dir.join("modbus_tools_test_log.csv");
        let _ = std::fs::remove_file(&path);
        let file = std::fs::File::create(&path).unwrap();
        let mut logger = Logger {
            file: std::io::BufWriter::new(file),
            cfg: LogCfg {
                path: path.to_string_lossy().to_string(),
                each_read: true,
                period_s: 1,
                delimiter: ',',
                on_change: false,
                timestamp: false,
            },
            last_write: None,
            last_flush: None,
            last_vals: None,
            header_written: false,
        };
        let rows = vec![
            DisplayRow {
                address: 0,
                value: "1".into(),
                raw: String::new(),
                num: Some(1.0),
            },
            DisplayRow {
                address: 1,
                value: "2".into(),
                raw: String::new(),
                num: Some(2.0),
            },
        ];
        logger.maybe_log(&rows).unwrap();
        drop(logger);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("@0,@1"), "header missing: {content}");
        assert!(content.contains("1,2"), "data row missing: {content}");
        let _ = std::fs::remove_file(&path);
    }

    async fn start_test_server(shared: Arc<SlaveShared>) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let svc = SlaveService {
            shared,
            unit_id: 1,
            tcp: true,
            ignore_unit_id: true,
        };
        tokio::spawn(async move {
            let on_connected = move |stream: tokio::net::TcpStream, peer: std::net::SocketAddr| {
                let svc = svc.clone();
                async move {
                    tokio_modbus::server::tcp::accept_tcp_connection(stream, peer, move |_| {
                        Ok(Some(svc.clone()))
                    })
                }
            };
            let _ = tokio_modbus::server::tcp::Server::new(listener)
                .serve(&on_connected, |_e: std::io::Error| {})
                .await;
        });
        addr
    }

    #[tokio::test]
    async fn raw_traffic_capture_and_scan_probe() {
        let shared = make_shared();
        shared.store.lock().unwrap().holding[0] = 0xBEEF;
        let addr = start_test_server(shared).await;
        let transport = Transport::Tcp {
            host: addr.ip().to_string(),
            port: addr.port(),
        };

        let (mut ctx, mut traffic, _) = connect_tapped(&transport, 1, None).await.unwrap();
        let v = ctx.read_holding_registers(0, 1).await.unwrap().unwrap();
        assert_eq!(v, vec![0xBEEF]);

        let mut tx_seen = false;
        let mut rx_seen = false;
        for _ in 0..20 {
            match traffic.rx.try_recv() {
                Ok((is_tx, bytes)) => {
                    assert!(!bytes.is_empty());
                    if is_tx {
                        tx_seen = true;
                    } else {
                        rx_seen = true;
                    }
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
        assert!(
            tx_seen && rx_seen,
            "expected raw Tx and Rx bytes to be captured"
        );

        let r = probe(&mut ctx, Area::HoldingRegisters, 0).await;
        assert!(r.is_ok(), "probe should succeed against a live server");
    }

    #[tokio::test]
    async fn tls_roundtrip() {
        let certs = test_certificates();
        let scfg = crate::tls::TlsServerCfg {
            cert_file: certs.server_cert.clone(),
            key_file: certs.server_key.clone(),
            ..Default::default()
        };
        let acceptor = crate::tls::server_acceptor(&scfg).unwrap();

        let shared = make_shared();
        shared.store.lock().unwrap().holding[0] = 0x1234;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let svc = SlaveService {
            shared,
            unit_id: 1,
            tcp: true,
            ignore_unit_id: true,
        };
        tokio::spawn(async move {
            let on_connected = move |stream: tokio::net::TcpStream, _peer: std::net::SocketAddr| {
                let svc = svc.clone();
                let acceptor = acceptor.clone();
                async move {
                    let tls = acceptor.accept(stream).await?;
                    Ok::<_, std::io::Error>(Some((svc.clone(), tls)))
                }
            };
            let _ = tokio_modbus::server::tcp::Server::new(listener)
                .serve(&on_connected, |_e: std::io::Error| {})
                .await;
        });

        // TLS client with skip-verify (the cert is self-signed).
        let ccfg = crate::tls::TlsClientCfg {
            ca_file: String::new(),
            skip_verify: true,
            domain: "localhost".into(),
            ..Default::default()
        };
        let connector = crate::tls::client_connector(&ccfg).unwrap();
        let name = crate::tls::server_name(&ccfg, "127.0.0.1").unwrap();
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let tls = connector.connect(name, stream).await.unwrap();
        let mut ctx = tcp::attach_slave(tls, Slave(1));
        let v = ctx.read_holding_registers(0, 1).await.unwrap().unwrap();
        assert_eq!(
            v,
            vec![0x1234],
            "Modbus read over TLS should return the stored value"
        );
    }

    #[tokio::test]
    async fn mutual_tls_roundtrip() {
        let certs = test_certificates();
        let scfg = crate::tls::TlsServerCfg {
            cert_file: certs.server_cert.clone(),
            key_file: certs.server_key.clone(),
            require_client_cert: true,
            client_ca: certs.ca_cert.clone(),
            ..Default::default()
        };
        let acceptor = crate::tls::server_acceptor(&scfg).unwrap();

        let shared = make_shared();
        shared.store.lock().unwrap().holding[0] = 0x55AA;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let svc = SlaveService {
            shared,
            unit_id: 1,
            tcp: true,
            ignore_unit_id: true,
        };
        tokio::spawn(async move {
            let on_connected = move |stream: tokio::net::TcpStream, _peer: std::net::SocketAddr| {
                let svc = svc.clone();
                let acceptor = acceptor.clone();
                async move {
                    let tls = acceptor.accept(stream).await?;
                    Ok::<_, std::io::Error>(Some((svc.clone(), tls)))
                }
            };
            let _ = tokio_modbus::server::tcp::Server::new(listener)
                .serve(&on_connected, |_e: std::io::Error| {})
                .await;
        });

        let ccfg = crate::tls::TlsClientCfg {
            ca_file: String::new(),
            skip_verify: true,
            domain: "localhost".into(),
            client_cert: certs.client_cert.clone(),
            client_key: certs.client_key.clone(),
            ..Default::default()
        };
        let connector = crate::tls::client_connector(&ccfg).unwrap();
        let name = crate::tls::server_name(&ccfg, "127.0.0.1").unwrap();
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let tls = connector.connect(name, stream).await.unwrap();
        let mut ctx = tcp::attach_slave(tls, Slave(1));
        let v = ctx.read_holding_registers(0, 1).await.unwrap().unwrap();
        assert_eq!(v, vec![0x55AA], "mutual-TLS Modbus read should succeed");
    }

    #[tokio::test]
    async fn tls_cipher_mismatch_fails() {
        let certs = test_certificates();
        // Server offers ONLY AES-256; client offers ONLY AES-128 → no shared suite.
        let scfg = crate::tls::TlsServerCfg {
            cert_file: certs.server_cert.clone(),
            key_file: certs.server_key.clone(),
            cipher: 2,
            ..Default::default()
        };
        let acceptor = crate::tls::server_acceptor(&scfg).unwrap();
        let shared = make_shared();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let svc = SlaveService {
            shared,
            unit_id: 1,
            tcp: true,
            ignore_unit_id: true,
        };
        tokio::spawn(async move {
            let on_connected = move |stream: tokio::net::TcpStream, _peer: std::net::SocketAddr| {
                let svc = svc.clone();
                let acceptor = acceptor.clone();
                async move {
                    let tls = acceptor.accept(stream).await?;
                    Ok::<_, std::io::Error>(Some((svc.clone(), tls)))
                }
            };
            let _ = tokio_modbus::server::tcp::Server::new(listener)
                .serve(&on_connected, |_e: std::io::Error| {})
                .await;
        });

        let ccfg = crate::tls::TlsClientCfg {
            skip_verify: true,
            domain: "localhost".into(),
            cipher: 1, // AES-128 only
            ..Default::default()
        };
        let connector = crate::tls::client_connector(&ccfg).unwrap();
        let name = crate::tls::server_name(&ccfg, "127.0.0.1").unwrap();
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        assert!(
            connector.connect(name, stream).await.is_err(),
            "AES-128-only client must fail to negotiate with an AES-256-only server"
        );
    }

    #[tokio::test]
    async fn tls_v13_negotiated() {
        let certs = test_certificates();
        let scfg = crate::tls::TlsServerCfg {
            cert_file: certs.server_cert.clone(),
            key_file: certs.server_key.clone(),
            version: 2, // TLS 1.3 only
            ..Default::default()
        };
        let acceptor = crate::tls::server_acceptor(&scfg).unwrap();
        let shared = make_shared();
        shared.store.lock().unwrap().holding[0] = 0x0BAD;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let svc = SlaveService {
            shared,
            unit_id: 1,
            tcp: true,
            ignore_unit_id: true,
        };
        tokio::spawn(async move {
            let on_connected = move |stream: tokio::net::TcpStream, _peer: std::net::SocketAddr| {
                let svc = svc.clone();
                let acceptor = acceptor.clone();
                async move {
                    let tls = acceptor.accept(stream).await?;
                    Ok::<_, std::io::Error>(Some((svc.clone(), tls)))
                }
            };
            let _ = tokio_modbus::server::tcp::Server::new(listener)
                .serve(&on_connected, |_e: std::io::Error| {})
                .await;
        });

        let ccfg = crate::tls::TlsClientCfg {
            skip_verify: true,
            domain: "localhost".into(),
            version: 2,
            ..Default::default()
        };
        let connector = crate::tls::client_connector(&ccfg).unwrap();
        let name = crate::tls::server_name(&ccfg, "127.0.0.1").unwrap();
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let tls = connector.connect(name, stream).await.unwrap();
        let desc = crate::tls::describe(tls.get_ref().1);
        assert!(desc.starts_with("TLS1.3"), "expected TLS1.3, got '{desc}'");
        let mut ctx = tcp::attach_slave(tls, Slave(1));
        assert_eq!(
            ctx.read_holding_registers(0, 1).await.unwrap().unwrap(),
            vec![0x0BAD]
        );
    }
}
