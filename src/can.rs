#![allow(clippy::drop_non_drop)]

// Pure CAN backend: compiled in pcanwork-core, independently from Slint UI code.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use crossbeam_channel::{
    Receiver as EventReceiver, Sender as EventChannelSender, TryRecvError, TrySendError, bounded,
};

static OTA_CANCEL: AtomicBool = AtomicBool::new(false);

use crate::dbc::DbcDb;
use crate::timestamp_quality::{TimestampQuality, TimestampQualitySnapshot};
use crate::vary::{self, VaryMode};

/// Maps a hardware clock into this process' monotonic capture timeline while preserving
/// device-level deltas. The first hardware sample is anchored to the host arrival time;
/// later samples no longer inherit UI/controller scheduling jitter.
#[derive(Debug)]
struct HardwareTimebase {
    tick_seconds: f64,
    wrap_modulus: Option<u64>,
    wrap_offset: u64,
    previous_raw: Option<u64>,
    anchor_raw: Option<u64>,
    anchor_host: f64,
}

impl HardwareTimebase {
    fn new(tick_seconds: f64, counter_bits: Option<u32>) -> Self {
        Self {
            tick_seconds,
            wrap_modulus: counter_bits.map(|bits| 1u64 << bits),
            wrap_offset: 0,
            previous_raw: None,
            anchor_raw: None,
            anchor_host: 0.0,
        }
    }

    fn map(&mut self, raw: u64, host_now: f64) -> f64 {
        if let (Some(modulus), Some(previous)) = (self.wrap_modulus, self.previous_raw)
            && raw < previous
            && previous - raw > modulus / 2
        {
            self.wrap_offset = self.wrap_offset.saturating_add(modulus);
        }
        self.previous_raw = Some(raw);
        let extended = self.wrap_offset.saturating_add(raw);
        if self.anchor_raw.is_none() {
            self.anchor_host = host_now;
            self.anchor_raw = Some(extended);
        }
        let anchor = self.anchor_raw.unwrap_or(extended);
        self.anchor_host + extended.saturating_sub(anchor) as f64 * self.tick_seconds
    }
}

pub fn cancel_ota() {
    OTA_CANCEL.store(true, Ordering::Relaxed);
}

#[derive(Clone, Debug)]
pub struct CanFrame {
    pub t: f64,
    pub ch: u8,
    pub tx: bool,
    pub id: u32,
    pub ext: bool,
    pub fd: bool, // CAN FD
    pub brs: bool,
    pub remote: bool,
    pub error: bool,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceConfig {
    pub sw_channel: u8,
    pub is_fd: bool,
    pub device_type: String,
    pub hardware_label: String,
    /// Stable physical endpoint identity. Device indices are only a runtime
    /// fallback because USB enumeration order may change after replugging.
    pub hardware_id: String,
    pub device_index: u32,
    pub channel_index: u32,
    pub baud: String,
    pub data_baud: String,
    /// Optional complete PCAN FD timing string (f_clock/nom_*/data_*).
    pub custom_bitrate: String,
    pub termination: bool,
    pub listen_only: bool,
    pub fd_non_iso: bool,
    pub net_server: bool,
    pub ip: String,
    pub port: String,
}

impl CanFrame {
    pub fn data_hex(&self) -> String {
        self.data
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Default)]
pub struct PollReport {
    pub receive_overruns: u64,
    pub driver_errors: u64,
    pub connection_lost: bool,
    pub message: Option<String>,
}

pub trait CanAdapter: Send {
    fn poll(&mut self, out: &mut Vec<CanFrame>) -> PollReport;
    fn send(&mut self, f: &CanFrame) -> Result<(), String>;
    fn name(&self) -> &str;
}

fn normalize_baud(baud: &str) -> String {
    let b = baud.trim().to_ascii_uppercase().replace(' ', "");
    match b.as_str() {
        "125" | "125KBPS" | "125K" => "125K".to_string(),
        "250" | "250KBPS" | "250K" => "250K".to_string(),
        "500" | "500KBPS" | "500K" => "500K".to_string(),
        "1000" | "1000KBPS" | "1000K" | "1M" | "1MBPS" => "1000K".to_string(),
        _ => b,
    }
}

fn zlg_timing(baud: &str) -> Option<(u8, u8)> {
    match normalize_baud(baud).as_str() {
        "1000K" => Some((0x00, 0x14)),
        "800K" => Some((0x00, 0x16)),
        "500K" => Some((0x00, 0x1C)),
        "250K" => Some((0x01, 0x1C)),
        "125K" => Some((0x03, 0x1C)),
        "100K" => Some((0x04, 0x1C)),
        "50K" => Some((0x09, 0x1C)),
        "20K" => Some((0x18, 0x1C)),
        "10K" => Some((0x31, 0x1C)),
        "5K" => Some((0xBF, 0xFF)),
        _ => None,
    }
}

#[path = "can/pcan.rs"]
mod pcan;
pub use pcan::*;

#[path = "can/vci.rs"]
mod vci;
pub use vci::*;

#[path = "can/zcan.rs"]
mod zcan;
pub use zcan::*;

pub enum Cmd {
    #[allow(dead_code)]
    Connect,
    #[allow(dead_code)]
    ConnectConfig(DeviceConfig),
    ConnectChannels(Vec<DeviceConfig>),
    Disconnect,
    Start,
    Stop,
    SendOnce(CanFrame),
    SendSequence {
        frame: CanFrame,
        count: u64,
        id_increment: bool,
        data_increment: bool,
    },
    SendBatch {
        frames: Vec<CanFrame>,
        repeat: u32,
        ack: Option<std::sync::mpsc::SyncSender<Result<u64, String>>>,
    },
    OtaRun(OtaJob),
    SetPeriodic {
        handle: u64,
        frame: CanFrame,
        period_ms: u64,
        repeat: i64,
        enable: bool,
    },
    StopPeriodic {
        handle: u64,
    },
    SetDynamicPeriodic {
        handle: u64,
        config: Option<DynamicPeriodicConfig>,
    },
    SetSimulationPeriodics(Vec<SimPeriodicConfig>),
    PlaybackLoad(Vec<CanFrame>),
    PlaybackPlay {
        online: bool,
        speed: f64,
        loop_play: bool,
    },
    PlaybackStep,
    PlaybackPause,
    PlaybackCancel,
    PlaybackSeek(f64),
    Shutdown,
}

#[derive(Clone)]
pub struct DynamicPeriodicConfig {
    pub frame: CanFrame,
    pub dbcs: Vec<DbcDb>,
    pub dbc_id: u32,
    pub signal_values: Vec<(String, f64)>,
    pub varies: Vec<(String, VaryMode)>,
    pub period_ms: u64,
    pub repeat: i64,
    pub start_sent: u64,
}

#[derive(Clone, Debug)]
pub enum SimGeneratorMode {
    Constant { value: f64 },
    Ramp { min: f64, max: f64, step: f64 },
    Sine { min: f64, max: f64 },
}

#[derive(Clone, Debug)]
pub struct SimSignalGenerator {
    pub signal: String,
    pub mode: SimGeneratorMode,
    pub period_ms: u64,
}

#[derive(Clone)]
pub struct SimPeriodicConfig {
    pub frame: CanFrame,
    pub dbc: Option<DbcDb>,
    pub dbc_id: u32,
    pub generators: Vec<SimSignalGenerator>,
}

#[derive(Clone, Debug)]
pub struct OtaJob {
    pub name: String,
    pub steps: Vec<OtaStep>,
    pub timeout_ms: u64,
    pub retries: u32,
}

#[derive(Clone, Debug)]
pub struct OtaStep {
    pub frame: CanFrame,
    pub ack: OtaAck,
    pub timeout_ms: u64,
    pub retries: u32,
}

#[derive(Clone, Copy, Debug)]
pub enum OtaResponseId {
    Exact(u32),
    WildcardBase(u32),
}

#[derive(Clone, Copy, Debug)]
pub enum OtaAck {
    None,
    XcpConnect { response: OtaResponseId },
    XcpAck { response: OtaResponseId },
    UdsFlowControl,
    UdsPositive { service: u8 },
}

pub enum Evt {
    Frame(CanFrame),
    Frames(Vec<CanFrame>),
    PlaybackFrame(CanFrame),
    PlaybackFrames(Vec<CanFrame>),
    Log(String),
    OtaProgress(usize, usize, String),
    Connected {
        channels: Vec<u8>,
        name: String,
        error: Option<String>,
    },
    Running(bool),
    Playback(usize, usize, bool),
    PeriodicDone(u64),
    PeriodicProgress {
        handle: u64,
        sent: u64,
    },
    DynamicUpdate {
        handle: u64,
        data: Vec<u8>,
        signal_values: Vec<(String, f64)>,
        sent: u64,
    },
    CaptureHealth {
        dropped_frames: u64,
        dropped_events: u64,
        hardware_overruns: u64,
        hardware_errors: u64,
        queue_depth: usize,
        queue_capacity: usize,
        queue_high_watermark: usize,
        command_rejected: u64,
        command_queue_depth: usize,
        command_queue_capacity: usize,
        command_queue_high_watermark: usize,
        timestamp_samples: u64,
        timestamp_latest_jitter_us: f64,
        timestamp_max_jitter_us: f64,
        timestamp_drift_ppm: f64,
        timestamp_monotonic_violations: u64,
    },
    ShutdownFinished,
}

const EVENT_QUEUE_CAPACITY: usize = 1024;
const EVENT_QUEUE_CONTROL_RESERVE: usize = 64;
const COMMAND_QUEUE_CAPACITY: usize = 512;

#[derive(Clone, Default)]
struct CommandHealth {
    rejected: Arc<AtomicU64>,
    high_watermark: Arc<AtomicUsize>,
    shutdown_requested: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct CommandSender {
    tx: EventChannelSender<Cmd>,
    health: CommandHealth,
}

#[derive(Clone, Copy, Debug)]
pub struct CommandRejected;

impl CommandSender {
    pub fn send(&self, command: Cmd) -> Result<(), CommandRejected> {
        if matches!(&command, Cmd::Shutdown) {
            self.health
                .shutdown_requested
                .store(true, Ordering::Release);
        }
        match self.tx.try_send(command) {
            Ok(()) => {
                self.health
                    .high_watermark
                    .fetch_max(self.tx.len(), Ordering::Relaxed);
                Ok(())
            }
            Err(_) => {
                self.health.rejected.fetch_add(1, Ordering::Relaxed);
                Err(CommandRejected)
            }
        }
    }

    pub fn send_critical(&self, command: Cmd, timeout: Duration) -> Result<(), CommandRejected> {
        if matches!(&command, Cmd::Shutdown) {
            self.health
                .shutdown_requested
                .store(true, Ordering::Release);
        }
        match self.tx.send_timeout(command, timeout) {
            Ok(()) => {
                self.health
                    .high_watermark
                    .fetch_max(self.tx.len(), Ordering::Relaxed);
                Ok(())
            }
            Err(_) => {
                self.health.rejected.fetch_add(1, Ordering::Relaxed);
                Err(CommandRejected)
            }
        }
    }
}

#[derive(Clone)]
struct EventSender {
    tx: EventChannelSender<Evt>,
    dropped_frames: Arc<AtomicU64>,
    dropped_events: Arc<AtomicU64>,
    high_watermark: Arc<AtomicUsize>,
    started: Instant,
    timestamp_quality: Arc<Mutex<HashMap<u8, TimestampQuality>>>,
}

impl EventSender {
    fn new(tx: EventChannelSender<Evt>) -> Self {
        Self {
            tx,
            dropped_frames: Arc::new(AtomicU64::new(0)),
            dropped_events: Arc::new(AtomicU64::new(0)),
            high_watermark: Arc::new(AtomicUsize::new(0)),
            started: Instant::now(),
            timestamp_quality: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn update_high_watermark(&self) {
        self.high_watermark
            .fetch_max(self.tx.len(), Ordering::Relaxed);
    }

    fn begin_timestamp_session(&self) {
        if let Ok(mut channels) = self.timestamp_quality.lock() {
            for quality in channels.values_mut() {
                quality.begin_session();
            }
        }
    }

    /// Non-blocking control delivery. Data producers leave a reserved tail in the queue so
    /// connection/error/shutdown state remains observable even during sustained bus load.
    fn send(&self, event: Evt) -> Result<(), ()> {
        match self.tx.try_send(event) {
            Ok(()) => {
                self.update_high_watermark();
                Ok(())
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.dropped_events.fetch_add(1, Ordering::Relaxed);
                Err(())
            }
        }
    }

    fn send_critical(&self, event: Evt, timeout: Duration) -> Result<(), ()> {
        match self.tx.send_timeout(event, timeout) {
            Ok(()) => {
                self.update_high_watermark();
                Ok(())
            }
            Err(_) => {
                self.dropped_events.fetch_add(1, Ordering::Relaxed);
                Err(())
            }
        }
    }

    /// Playback frames are lossless while replaying offline: a full queue is reported as
    /// backpressure so the controller retries the same frame on its next slice. Online
    /// replay cannot retry after the hardware transmission without duplicating a bus frame.
    fn try_send_playback_frame(&self, frame: CanFrame) -> PlaybackEventSend {
        match self.tx.try_send(Evt::PlaybackFrame(frame)) {
            Ok(()) => {
                self.update_high_watermark();
                PlaybackEventSend::Enqueued
            }
            Err(TrySendError::Full(_)) => PlaybackEventSend::Full,
            Err(TrySendError::Disconnected(_)) => {
                self.dropped_events.fetch_add(1, Ordering::Relaxed);
                PlaybackEventSend::Disconnected
            }
        }
    }

    fn try_send_playback_frames(&self, frames: Vec<CanFrame>) -> PlaybackEventSend {
        if frames.is_empty() {
            return PlaybackEventSend::Enqueued;
        }
        match self.tx.try_send(Evt::PlaybackFrames(frames)) {
            Ok(()) => {
                self.update_high_watermark();
                PlaybackEventSend::Enqueued
            }
            Err(TrySendError::Full(_)) => PlaybackEventSend::Full,
            Err(TrySendError::Disconnected(_)) => {
                self.dropped_events.fetch_add(1, Ordering::Relaxed);
                PlaybackEventSend::Disconnected
            }
        }
    }

    /// Capture frames are deliberately dropped as a whole batch before they can block the
    /// hardware polling loop. Every dropped frame is counted and reported to the UI.
    fn send_frames(&self, frames: Vec<CanFrame>) {
        if frames.is_empty() {
            return;
        }
        let host_receive_s = self.started.elapsed().as_secs_f64();
        if let Ok(mut quality) = self.timestamp_quality.lock() {
            for frame in &frames {
                quality
                    .entry(frame.ch)
                    .or_default()
                    .observe(frame.t, host_receive_s);
            }
        }
        let count = frames.len() as u64;
        if self.tx.len() >= EVENT_QUEUE_CAPACITY - EVENT_QUEUE_CONTROL_RESERVE {
            self.dropped_frames.fetch_add(count, Ordering::Relaxed);
            return;
        }
        match self.tx.try_send(Evt::Frames(frames)) {
            Ok(()) => self.update_high_watermark(),
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.dropped_frames.fetch_add(count, Ordering::Relaxed);
            }
        }
    }

    fn report_health(
        &self,
        hardware_overruns: u64,
        hardware_errors: u64,
        commands: &CommandHealth,
        command_depth: usize,
    ) {
        let timestamp = self.timestamp_quality_snapshot();
        let event = Evt::CaptureHealth {
            dropped_frames: self.dropped_frames.load(Ordering::Relaxed),
            dropped_events: self.dropped_events.load(Ordering::Relaxed),
            hardware_overruns,
            hardware_errors,
            queue_depth: self.tx.len(),
            queue_capacity: EVENT_QUEUE_CAPACITY,
            queue_high_watermark: self.high_watermark.load(Ordering::Relaxed),
            command_rejected: commands.rejected.load(Ordering::Relaxed),
            command_queue_depth: command_depth,
            command_queue_capacity: COMMAND_QUEUE_CAPACITY,
            command_queue_high_watermark: commands.high_watermark.load(Ordering::Relaxed),
            timestamp_samples: timestamp.samples,
            timestamp_latest_jitter_us: timestamp.latest_transport_jitter_us,
            timestamp_max_jitter_us: timestamp.max_transport_jitter_us,
            timestamp_drift_ppm: timestamp.clock_drift_ppm,
            timestamp_monotonic_violations: timestamp.monotonic_violations,
        };
        if self.tx.try_send(event).is_ok() {
            self.update_high_watermark();
        }
    }

    fn timestamp_quality_snapshot(&self) -> TimestampQualitySnapshot {
        let Ok(channels) = self.timestamp_quality.lock() else {
            return TimestampQualitySnapshot::default();
        };
        let mut aggregate = TimestampQualitySnapshot::default();
        for quality in channels.values() {
            let snapshot = quality.snapshot();
            aggregate.samples += snapshot.samples;
            aggregate.latest_transport_jitter_us = aggregate
                .latest_transport_jitter_us
                .max(snapshot.latest_transport_jitter_us);
            aggregate.max_transport_jitter_us = aggregate
                .max_transport_jitter_us
                .max(snapshot.max_transport_jitter_us);
            if snapshot.clock_drift_ppm.abs() > aggregate.clock_drift_ppm.abs() {
                aggregate.clock_drift_ppm = snapshot.clock_drift_ppm;
            }
            aggregate.monotonic_violations += snapshot.monotonic_violations;
        }
        aggregate
    }
}

struct Playback {
    frames: Vec<CanFrame>,
    idx: usize,
    online: bool,
    speed: f64,
    playing: bool,
    paused: bool,
    base: Instant,
    base_t: f64,
    loop_play: bool,
}

struct Periodic {
    frame: CanFrame,
    period: Duration,
    next: Instant,
    remaining: i64,
    sent: u64,
}

struct DynamicPeriodic {
    config: DynamicPeriodicConfig,
    next: Instant,
    sent: u64,
}

struct SimSignalState {
    config: SimSignalGenerator,
    next: Instant,
    tick: u64,
}

struct SimPeriodic {
    frame: CanFrame,
    dbc: Option<DbcDb>,
    dbc_id: u32,
    generators: Vec<SimSignalState>,
    failed: bool,
}

fn sim_generator_value(generator: &SimSignalState) -> f64 {
    match generator.config.mode {
        SimGeneratorMode::Constant { value } => value,
        SimGeneratorMode::Ramp { min, max, step } => {
            let span = (max - min).abs().max(1e-9);
            let step = step.abs().max(1e-9);
            let pos = (generator.tick as f64 * step) % (2.0 * span);
            if pos <= span {
                min + pos
            } else {
                min + 2.0 * span - pos
            }
        }
        SimGeneratorMode::Sine { min, max } => {
            min + (max - min) * (0.5 + 0.5 * (generator.tick as f64 * 0.2).sin())
        }
    }
}

fn update_sim_periodic(periodic: &mut SimPeriodic, now: Instant) -> Result<bool, String> {
    let mut changed = false;
    for generator in &mut periodic.generators {
        if now < generator.next {
            continue;
        }
        let period = Duration::from_millis(generator.config.period_ms.max(10));
        generator.next += period;
        if generator.next <= now {
            generator.next = now + period;
        }
        let value = sim_generator_value(generator);
        generator.tick = generator.tick.wrapping_add(1);
        if generator.config.signal.is_empty() {
            if let Some(first) = periodic.frame.data.first_mut() {
                *first = value.clamp(0.0, 255.0) as u8;
            }
        } else if let Some(dbc) = periodic.dbc.as_ref() {
            periodic.frame.data = dbc.encode_signal_into_ext(
                periodic.dbc_id,
                periodic.frame.ext,
                &periodic.frame.data,
                &generator.config.signal,
                value,
            )?;
        } else {
            return Err("DBC signal generator has no DBC database".into());
        }
        changed = true;
    }
    Ok(changed)
}

#[path = "can/send_queue.rs"]
mod send_queue;
use send_queue::*;
#[path = "can/precise_wait.rs"]
mod precise_wait;
use precise_wait::*;

/// Advances a form's sequence seed past the frames that were just queued.
/// This keeps repeated one-frame sends continuous instead of restarting from
/// the same ID and payload on every click.
pub fn advance_send_sequence_seed(
    frame: &mut CanFrame,
    steps: u64,
    id_increment: bool,
    data_increment: bool,
) {
    advance_sequence_frame(frame, steps, id_increment, data_increment);
}

fn dynamic_rand01(seed: u64) -> f64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / ((1u64 << 53) as f64)
}

fn build_dynamic_frame(
    handle: u64,
    periodic: &DynamicPeriodic,
) -> Result<(CanFrame, Vec<(String, f64)>), String> {
    let mut values: HashMap<String, f64> = periodic.config.signal_values.iter().cloned().collect();
    for (signal, mode) in &periodic.config.varies {
        let base = values.get(signal).copied().unwrap_or(0.0);
        let mut seed = handle ^ periodic.sent.wrapping_mul(0x0100_0001);
        for byte in signal.bytes() {
            seed = seed.wrapping_mul(31).wrapping_add(byte as u64);
        }
        values.insert(
            signal.clone(),
            vary::eval(mode, periodic.sent, base, dynamic_rand01(seed)),
        );
    }
    let mut frame = periodic.config.frame.clone();
    frame.data = periodic
        .config
        .dbcs
        .iter()
        .find_map(|dbc| dbc.encode_ext(periodic.config.dbc_id, frame.ext, &values))
        .ok_or_else(|| {
            format!(
                "DBC message 0x{:X} ext={} not found for dynamic send",
                periodic.config.dbc_id, frame.ext
            )
        })?;
    Ok((frame, values.into_iter().collect()))
}

pub fn spawn() -> (CommandSender, EventReceiver<Evt>) {
    let (cmd_tx, cmd_rx) = bounded::<Cmd>(COMMAND_QUEUE_CAPACITY);
    let (evt_tx, evt_rx) = bounded::<Evt>(EVENT_QUEUE_CAPACITY);
    let command_health = CommandHealth::default();
    let sender = CommandSender {
        tx: cmd_tx,
        health: command_health.clone(),
    };
    std::thread::spawn(move || controller(cmd_rx, EventSender::new(evt_tx), command_health));
    (sender, evt_rx)
}

fn open_adapter(start: Instant, cfg: &DeviceConfig) -> Result<Box<dyn CanAdapter>, String> {
    let device = cfg.device_type.trim().to_ascii_uppercase();
    if device == "VIRTUAL" || device == "SIM" {
        Err("不支持的设备类型: 虚拟总线已移除".into())
    } else if device == "PCAN" {
        PcanBus::open_cfg(start, cfg).map(|b| Box::new(b) as Box<dyn CanAdapter>)
    } else if device == "GCAN" {
        VciBus::open(start, cfg, &["ECanVci64.dll", "ECanVci.dll"], "", 3)
            .map(|b| Box::new(b) as Box<dyn CanAdapter>)
    } else if device == "ZHCX" || device == "ZHCXCAN" {
        VciBus::open(start, cfg, &["ControlCAN.dll"], "VCI_", 4)
            .map(|b| Box::new(b) as Box<dyn CanAdapter>)
    } else if zcan_profile(&cfg.device_type).is_some() {
        ZcanFdBus::open(start, cfg).map(|b| Box::new(b) as Box<dyn CanAdapter>)
    } else {
        Err(format!("未知设备类型: {}", cfg.device_type))
    }
}

fn send_on(adapters: &mut [(u8, Box<dyn CanAdapter>)], f: &CanFrame) -> Result<u8, String> {
    let (channel, adapter) = adapters
        .iter_mut()
        .find(|(channel, _)| *channel == f.ch)
        .ok_or_else(|| format!("CAN 通道 {} 未连接", f.ch))?;
    adapter.send(f).map(|()| *channel)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlaybackFrameEmit {
    Emitted,
    Backpressure,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlaybackEventSend {
    Enqueued,
    Full,
    Disconnected,
}

fn emit_playback_frame(
    adapters: &mut [(u8, Box<dyn CanAdapter>)],
    evt_tx: &EventSender,
    mut frame: CanFrame,
    online: bool,
) -> PlaybackFrameEmit {
    if online {
        match send_on(adapters, &frame) {
            Ok(channel) => {
                frame.ch = channel;
                frame.tx = true;
            }
            Err(error) => {
                let _ = evt_tx.send(Evt::Log(format!("在线回放发送失败: {error}")));
                return PlaybackFrameEmit::Failed;
            }
        }
    }
    match evt_tx.try_send_playback_frame(frame) {
        PlaybackEventSend::Enqueued => PlaybackFrameEmit::Emitted,
        PlaybackEventSend::Full if online => {
            // The bus frame was already sent. Only its UI echo is dropped; retrying here
            // would transmit a duplicate frame on the physical bus.
            evt_tx.dropped_events.fetch_add(1, Ordering::Relaxed);
            PlaybackFrameEmit::Emitted
        }
        PlaybackEventSend::Full => PlaybackFrameEmit::Backpressure,
        PlaybackEventSend::Disconnected => PlaybackFrameEmit::Failed,
    }
}

fn connect_channels(
    adapters: &mut Vec<(u8, Box<dyn CanAdapter>)>,
    running: &mut bool,
    periodics: &mut HashMap<u64, Periodic>,
    evt_tx: &EventSender,
    start: Instant,
    cfgs: Vec<DeviceConfig>,
) {
    if let Err(error) = validate_channel_set(&cfgs) {
        let _ = evt_tx.send(Evt::Log(format!("CAN 通道配置无效: {error}")));
        let names = adapters
            .iter()
            .map(|(channel, adapter)| format!("CAN{channel}:{}", adapter.name()))
            .collect::<Vec<_>>()
            .join("  ");
        let _ = evt_tx.send(Evt::Connected {
            channels: adapters.iter().map(|(channel, _)| *channel).collect(),
            name: names,
            error: Some(error),
        });
        return;
    }
    *running = false;
    periodics.clear();
    adapters.clear();
    let _ = evt_tx.send(Evt::Running(false));
    let mut names: Vec<String> = Vec::new();
    let mut failures = Vec::new();
    for cfg in &cfgs {
        let ch = if cfg.sw_channel == 0 {
            1
        } else {
            cfg.sw_channel
        };
        match open_adapter(start, cfg) {
            Ok(bus) => {
                let name = bus.name().to_string();
                adapters.push((ch, bus));
                let _ = evt_tx.send(Evt::Log(format!("CAN{ch} 已连接: {name}")));
                names.push(format!("CAN{ch}:{name}"));
            }
            Err(e) => {
                let _ = evt_tx.send(Evt::Log(format!("CAN{ch} 连接失败: {e}")));
                failures.push(format!("CAN{ch}: {e}"));
            }
        }
    }
    if !failures.is_empty() {
        adapters.clear();
        let _ = evt_tx.send(Evt::Log(format!(
            "多通道连接已回滚，所有通道均保持断开: {}",
            failures.join("；")
        )));
        let _ = evt_tx.send(Evt::Connected {
            channels: Vec::new(),
            name: String::new(),
            error: Some(failures.join("；")),
        });
    } else if adapters.is_empty() {
        let _ = evt_tx.send(Evt::Connected {
            channels: Vec::new(),
            name: String::new(),
            error: Some("没有可连接的 CAN 通道".into()),
        });
    } else {
        let _ = evt_tx.send(Evt::Connected {
            channels: adapters.iter().map(|(channel, _)| *channel).collect(),
            name: names.join("  "),
            error: None,
        });
    }
}

fn ota_response_id_matches(spec: OtaResponseId, id: u32) -> bool {
    match spec {
        OtaResponseId::Exact(expected) => id == expected,
        OtaResponseId::WildcardBase(base) => (id & 0xFFFF_FF00) == base,
    }
}

fn ota_ack_matches(ack: OtaAck, frame: &CanFrame) -> bool {
    match ack {
        OtaAck::None => true,
        OtaAck::XcpConnect { response } => {
            ota_response_id_matches(response, frame.id)
                && frame.data.len() >= 8
                && frame.data[0] == 0xFF
                && frame.data[1] == 0x10
                && frame.data[4] == 0x08
                && frame.data[5] == 0x00
                && frame.data[6] == 0x01
                && frame.data[7] == 0x01
        }
        OtaAck::XcpAck { response } => {
            ota_response_id_matches(response, frame.id)
                && frame.data.len() == 1
                && frame.data[0] == 0xFF
        }
        OtaAck::UdsFlowControl => frame.data.first().copied() == Some(0x30),
        OtaAck::UdsPositive { service } => {
            frame.data.get(1).copied() == Some(service.wrapping_add(0x40))
        }
    }
}

fn ota_ack_matches_on_channel(
    ack: OtaAck,
    expected_channel: u8,
    actual_channel: u8,
    frame: &CanFrame,
) -> bool {
    actual_channel == expected_channel && ota_ack_matches(ack, frame)
}

fn poll_for_ota_ack(
    adapters: &mut [(u8, Box<dyn CanAdapter>)],
    evt_tx: &EventSender,
    buf: &mut Vec<CanFrame>,
    ack: OtaAck,
    expected_channel: u8,
    timeout: Duration,
) -> bool {
    if matches!(ack, OtaAck::None) {
        return true;
    }
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if OTA_CANCEL.load(Ordering::Relaxed) {
            return false;
        }
        for (ch, adapter) in adapters.iter_mut() {
            buf.clear();
            let _ = adapter.poll(buf);
            for mut frame in buf.drain(..) {
                frame.ch = *ch;
                let matched = ota_ack_matches_on_channel(ack, expected_channel, *ch, &frame);
                let _ = evt_tx.send(Evt::Frame(frame));
                if matched {
                    return true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    false
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    #[test]
    fn controlcan_board_info_matches_vendor_abi() {
        assert_eq!(std::mem::size_of::<zlg_ffi::VCI_BOARD_INFO>(), 80);
        assert_eq!(std::mem::size_of::<zlg_ffi::VCI_CAN_STATUS>(), 12);
        assert_eq!(std::mem::size_of::<zlg_ffi::VCI_CAN_OBJ>(), 24);
        assert_eq!(std::mem::size_of::<zlg_ffi::VCI_INIT_CONFIG>(), 16);
        assert_eq!(printable_vci_text(b"CANalyst-II\0ignored"), "CANalyst-II");
    }

    #[test]
    #[ignore = "requires locally attached GCAN/ZHCX hardware"]
    fn attached_usb_can_hardware_probe() {
        for device in zcan_attached_channels() {
            println!(
                "{} dev{} CAN{} {} SN {}",
                device.device_type,
                device.device_index,
                device.channel_index + 1,
                device.hardware_label,
                device.serial_number
            );
        }
    }

    #[test]
    #[ignore = "requires GCAN USBCAN-I and CANalyst-II on one 500K bus"]
    fn shared_vci_three_channel_hardware_matrix() {
        clear_vci_device_registry();
        let start = Instant::now();
        let gcan = device("GCAN", 1);
        let mut zhcx_can1 = device("ZHCX", 2);
        zhcx_can1.channel_index = 0;
        let mut zhcx_can2 = device("ZHCX", 3);
        zhcx_can2.channel_index = 1;
        let configs = [gcan, zhcx_can1, zhcx_can2];
        let mut adapters = configs
            .iter()
            .map(|config| (config.sw_channel, open_adapter(start, config).unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(vci_device_registry().lock().unwrap().len(), 2);
        std::thread::sleep(Duration::from_millis(100));

        for source in 0..adapters.len() {
            let id = 0x6E1 + source as u32;
            let frame = CanFrame {
                t: 0.0,
                ch: adapters[source].0,
                tx: true,
                id,
                ext: false,
                fd: false,
                brs: false,
                remote: false,
                error: false,
                data: vec![0xA0 + source as u8, 1, 2, 3, 4, 5, 6, 7],
            };
            adapters[source].1.send(&frame).unwrap();
            let deadline = Instant::now() + Duration::from_millis(500);
            let mut received = vec![false; adapters.len()];
            while Instant::now() < deadline {
                for (target, (_, adapter)) in adapters.iter_mut().enumerate() {
                    let mut frames = Vec::new();
                    let report = adapter.poll(&mut frames);
                    assert!(!report.connection_lost, "{:?}", report.message);
                    if target != source && frames.iter().any(|candidate| candidate.id == id) {
                        received[target] = true;
                    }
                }
                if received
                    .iter()
                    .enumerate()
                    .all(|(target, hit)| target == source || *hit)
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                received
                    .iter()
                    .enumerate()
                    .all(|(target, hit)| target == source || *hit),
                "source={} receive matrix={received:?}",
                source
            );
        }

        adapters.clear();
        assert_eq!(vci_device_registry().lock().unwrap().len(), 2);
        clear_vci_device_registry();
        assert!(vci_device_registry().lock().unwrap().is_empty());
    }

    fn device(device_type: &str, channel: u8) -> DeviceConfig {
        DeviceConfig {
            sw_channel: channel,
            is_fd: false,
            device_type: device_type.into(),
            hardware_label: String::new(),
            hardware_id: String::new(),
            device_index: 0,
            channel_index: (channel - 1) as u32,
            baud: "500K".into(),
            data_baud: "2M".into(),
            custom_bitrate: String::new(),
            termination: false,
            listen_only: false,
            fd_non_iso: false,
            net_server: false,
            ip: "192.168.0.10".into(),
            port: "8000".into(),
        }
    }

    #[test]
    fn pcan_unplug_status_is_classified_as_connection_loss() {
        let report = pcan_poll_error(pcan_ffi::PCAN_ERROR_ILLHW);
        assert!(report.connection_lost);
        let bus_off = pcan_poll_error(pcan_ffi::PCAN_ERROR_BUSOFF);
        assert!(!bus_off.connection_lost);
    }

    #[test]
    fn connection_loss_requires_three_consecutive_reports() {
        let mut streaks = HashMap::new();
        assert!(!connection_loss_confirmed(&mut streaks, 1, true));
        assert!(!connection_loss_confirmed(&mut streaks, 1, false));
        assert!(!connection_loss_confirmed(&mut streaks, 1, true));
        assert!(!connection_loss_confirmed(&mut streaks, 1, false));
        assert!(connection_loss_confirmed(&mut streaks, 1, true));
    }

    #[test]
    fn channel_validation_is_adapter_aware() {
        assert!(validate_device_config(&device("PCAN", 1)).is_ok());

        let mut classic = device("USBCAN-E-U", 1);
        classic.is_fd = true;
        assert!(validate_device_config(&classic).is_err());

        let mut network = device("CANFDNET", 1);
        network.is_fd = true;
        network.ip = "not-an-ip".into();
        assert!(validate_device_config(&network).is_err());
        network.ip = "192.168.0.10".into();
        assert!(validate_device_config(&network).is_ok());
    }

    #[test]
    fn zlg_e_u_and_canfd_mini_use_distinct_driver_families() {
        let usbcan1 = zcan_profile("USBCAN1").unwrap();
        let usbcan2 = zcan_profile("USBCAN2").unwrap();
        let eu = zcan_profile("USBCAN-E-U").unwrap();
        let two_eu = zcan_profile("USBCAN-2E-U").unwrap();
        let mini = zcan_profile("USBCANFD-MINI").unwrap();
        assert_eq!(usbcan1.device_type, 3);
        assert_eq!(usbcan2.device_type, 4);
        assert_eq!(usbcan1.family, ZcanDeviceFamily::UsbClassic);
        assert_eq!(usbcan2.family, ZcanDeviceFamily::UsbClassic);
        assert_eq!(eu.device_type, 20);
        assert_eq!(two_eu.device_type, 21);
        assert_eq!(eu.family, ZcanDeviceFamily::UsbClassic);
        assert!(!eu.fd_capable);
        assert_eq!(mini.device_type, 43);
        assert_eq!(mini.family, ZcanDeviceFamily::UsbCanFd);
        assert!(mini.fd_capable);
    }

    #[test]
    fn zlg_canfd_hardware_uses_canfd_driver_channel_for_classic_frames() {
        let profile = zcan_profile("USBCANFD-200U").unwrap();
        assert_eq!(profile.family, ZcanDeviceFamily::UsbCanFd);
        assert!(profile.fd_capable);
        // ZLG's CAN FD USB kernel rejects ZCAN_StartCAN after TYPE_CAN init.
        // Classic-vs-FD remains a frame policy; it is not the driver channel type.
        let driver_type = zcan_driver_channel_type(profile, false);
        assert_eq!(driver_type, zcan_ffi::TYPE_CANFD);
    }

    #[test]
    fn zlg_usb_shared_device_key_ignores_stale_network_fields() {
        let profile = zcan_profile("USBCANFD-200U").unwrap();
        let first = device("USBCANFD-200U", 1);
        let mut second = device("USBCANFD-200U", 2);
        second.channel_index = 1;
        second.net_server = true;
        second.ip = "192.168.0.178".into();
        second.port = "8000".into();
        assert_eq!(
            zcan_device_key(profile, &first),
            zcan_device_key(profile, &second)
        );
    }

    #[test]
    fn zlg_e_u_classic_init_uses_sja1000_timing_layout() {
        use zcan_ffi::{ZcanChannelConfig, ZcanChannelInitConfig, ZcanClassicInitConfig};

        let (timing0, timing1) = zlg_timing("500K").unwrap();
        let cfg = ZcanChannelInitConfig {
            can_type: 0,
            config: ZcanChannelConfig {
                classic: ZcanClassicInitConfig {
                    acc_code: 0,
                    acc_mask: 0xFFFF_FFFF,
                    reserved: 0,
                    filter: 1,
                    timing0,
                    timing1,
                    mode: 0,
                },
            },
        };
        let classic = unsafe { cfg.config.classic };
        assert_eq!(std::mem::size_of::<ZcanChannelInitConfig>(), 32);
        assert_eq!(classic.timing0, timing0);
        assert_eq!(classic.timing1, timing1);
        assert_eq!(classic.acc_mask, 0xFFFF_FFFF);
    }

    #[test]
    fn zlg_bus_error_is_actionable_but_not_a_usb_disconnect() {
        use zcan_ffi::{ERROR_CAN_BUSERR, ERROR_CAN_BUSOFF, ZcanChannelStatus};
        let status = ZcanChannelStatus {
            reg_re_counter: 7,
            reg_te_counter: 31,
            ..Default::default()
        };
        let message = zcan_error_message(ERROR_CAN_BUSERR | ERROR_CAN_BUSOFF, Some(status));
        assert!(message.contains("CAN_H/CAN_L"));
        assert!(message.contains("Bus-Off"));
        assert!(message.contains("RXErr=7"));
        assert!(!zcan_error_is_connection_lost(
            ERROR_CAN_BUSERR | ERROR_CAN_BUSOFF
        ));
    }

    #[test]
    fn send_sequence_increments_id_and_little_endian_payload() {
        let mut source = frame(1);
        source.id = 0x7FE;
        source.data = vec![0xFF, 0x00];
        let mut job = PendingSendJob::sequence(source, 3, true, true).unwrap();
        let first = job.next_frame().unwrap();
        let second = job.next_frame().unwrap();
        let third = job.next_frame().unwrap();
        assert_eq!((first.id, first.data), (0x7FE, vec![0xFF, 0x00]));
        assert_eq!((second.id, second.data), (0x7FF, vec![0x00, 0x01]));
        assert_eq!((third.id, third.data), (0x000, vec![0x01, 0x01]));
        assert_eq!(job.remaining(), 0);
    }

    #[test]
    fn completed_sequence_advances_the_next_form_seed() {
        let mut seed = frame(1);
        seed.id = 0x100;
        seed.data = vec![0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77];

        advance_send_sequence_seed(&mut seed, 1, true, true);

        assert_eq!(seed.id, 0x101);
        assert_eq!(
            seed.data,
            vec![0x01, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77]
        );
    }

    #[test]
    fn form_seed_advances_by_the_entire_queued_sequence() {
        let mut seed = frame(1);
        seed.id = 0x7FE;
        seed.data = vec![0xFF, 0x00];

        advance_send_sequence_seed(&mut seed, 3, true, true);

        assert_eq!(seed.id, 0x001);
        assert_eq!(seed.data, vec![0x02, 0x01]);
    }

    #[test]
    fn form_seed_increment_options_are_independent() {
        let mut id_only = frame(1);
        id_only.id = 0x100;
        id_only.data = vec![0x10];
        advance_send_sequence_seed(&mut id_only, 1, true, false);
        assert_eq!((id_only.id, id_only.data), (0x101, vec![0x10]));

        let mut data_only = frame(1);
        data_only.id = 0x100;
        data_only.data = vec![0x10];
        advance_send_sequence_seed(&mut data_only, 1, false, true);
        assert_eq!((data_only.id, data_only.data), (0x100, vec![0x11]));
    }

    #[test]
    fn stop_periodic_removes_static_and_dynamic_jobs_for_the_handle() {
        let now = Instant::now();
        let mut periodics = HashMap::from([(
            7,
            Periodic {
                frame: frame(1),
                period: Duration::from_millis(10),
                next: now,
                remaining: 1000,
                sent: 67,
            },
        )]);
        let mut dynamic_periodics = HashMap::from([(
            7,
            DynamicPeriodic {
                config: DynamicPeriodicConfig {
                    frame: frame(1),
                    dbcs: Vec::new(),
                    dbc_id: 0x116,
                    signal_values: Vec::new(),
                    varies: Vec::new(),
                    period_ms: 10,
                    repeat: 1000,
                    start_sent: 67,
                },
                next: now,
                sent: 67,
            },
        )]);

        controller::stop_periodic_jobs(&mut periodics, &mut dynamic_periodics, 7);

        assert!(!periodics.contains_key(&7));
        assert!(!dynamic_periodics.contains_key(&7));
    }

    #[test]
    fn periodic_deadline_does_not_accumulate_wakeup_delay() {
        let scheduled = Instant::now();
        let period = Duration::from_millis(10);
        let woke_late = scheduled + Duration::from_millis(2);

        let next = controller::advance_periodic_deadline(scheduled, period, woke_late);

        assert_eq!(next, scheduled + period);
    }

    #[test]
    fn periodic_deadline_skips_expired_slots_after_an_overrun() {
        let scheduled = Instant::now();
        let period = Duration::from_millis(10);
        let woke_after_next_slot = scheduled + Duration::from_millis(12);

        let next = controller::advance_periodic_deadline(scheduled, period, woke_after_next_slot);

        assert_eq!(next, woke_after_next_slot + period);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "host timing diagnostic; run explicitly"]
    fn windows_precise_waiter_timing_diagnostic() {
        let waiter = PreciseWaiter::new();
        for period_ms in [1u64, 10] {
            let period = Duration::from_millis(period_ms);
            let samples = 200usize;
            let base = Instant::now();
            let mut previous = base;
            let mut errors_ms = Vec::with_capacity(samples);
            let mut total_interval_ms = 0.0;
            for slot in 1..=samples {
                waiter.wait_until(base + period * slot as u32);
                let now = Instant::now();
                let interval_ms = now.duration_since(previous).as_secs_f64() * 1000.0;
                total_interval_ms += interval_ms;
                errors_ms.push((interval_ms - period_ms as f64).abs());
                previous = now;
            }
            errors_ms.sort_by(f64::total_cmp);
            let p95 = errors_ms[(samples * 95 / 100).min(samples - 1)];
            let max = errors_ms[samples - 1];
            println!(
                "period={period_ms}ms mean={:.4}ms p95_abs_error={p95:.4}ms max_abs_error={max:.4}ms",
                total_interval_ms / samples as f64
            );
        }
    }

    #[test]
    fn pending_send_queue_rejects_unbounded_work() {
        let mut queue = VecDeque::new();
        let job =
            PendingSendJob::sequence(frame(1), MAX_PENDING_SEND_FRAMES, false, false).unwrap();
        assert_eq!(
            enqueue_send_job(&mut queue, job),
            Ok(MAX_PENDING_SEND_FRAMES)
        );
        let extra = PendingSendJob::sequence(frame(1), 1, false, false).unwrap();
        assert!(enqueue_send_job(&mut queue, extra).is_err());
    }

    #[test]
    fn channel_set_rejects_duplicate_software_and_hardware_bindings() {
        let first = device("PCAN", 1);
        let mut duplicate_software = device("PCAN", 1);
        duplicate_software.channel_index = 1;
        assert!(validate_channel_set(&[first.clone(), duplicate_software]).is_err());

        let mut duplicate_hardware = first.clone();
        duplicate_hardware.sw_channel = 2;
        assert!(validate_channel_set(&[first, duplicate_hardware]).is_err());
    }

    #[test]
    fn stable_hardware_identity_survives_runtime_index_changes() {
        let mut first = device("USBCANFD-200U", 1);
        first.is_fd = true;
        first.hardware_id = "USBCANFD-200U:46716A0:0".into();

        let mut same_endpoint = first.clone();
        same_endpoint.sw_channel = 2;
        same_endpoint.device_index = 7;
        same_endpoint.channel_index = 1;

        let error = validate_channel_set(&[first, same_endpoint]).unwrap_err();
        assert!(error.contains("同一硬件端点"));
    }

    #[test]
    fn pcan_fd_accepts_complete_custom_timing_and_rejects_partial_text() {
        let mut config = device("PCAN", 1);
        config.is_fd = true;
        config.custom_bitrate = "f_clock=80000000,nom_brp=2,nom_tseg1=63,nom_tseg2=16,nom_sjw=16,data_brp=2,data_tseg1=15,data_tseg2=4,data_sjw=4".into();
        assert!(validate_device_config(&config).is_ok());

        config.custom_bitrate = "nominal 500K / data 2M".into();
        assert!(validate_device_config(&config).is_err());
    }

    #[test]
    fn capability_validation_rejects_incompatible_modes() {
        let mut classic = device("GCAN", 1);
        classic.fd_non_iso = true;
        assert!(validate_device_config(&classic).is_err());

        let mut fd = device("USBCANFD-200U", 1);
        fd.is_fd = true;
        fd.fd_non_iso = true;
        assert!(validate_device_config(&fd).is_ok());

        let mut pcan = device("PCAN", 1);
        pcan.listen_only = true;
        assert!(validate_device_config(&pcan).is_err());
    }

    #[test]
    fn hardware_timebase_preserves_device_deltas() {
        let mut clock = HardwareTimebase::new(1e-6, None);
        assert!((clock.map(1_000_000, 5.0) - 5.0).abs() < 1e-12);
        assert!((clock.map(1_250_000, 8.0) - 5.25).abs() < 1e-12);
    }

    #[test]
    fn hardware_timebase_extends_32_bit_wrap() {
        let mut clock = HardwareTimebase::new(1e-4, Some(32));
        let before_wrap = u32::MAX as u64 - 4;
        assert!((clock.map(before_wrap, 2.0) - 2.0).abs() < 1e-12);
        assert!((clock.map(5, 9.0) - 2.001).abs() < 1e-12);
    }

    #[test]
    fn event_queue_drops_capture_before_control_reserve_and_reports_it() {
        let (raw_tx, rx) = bounded(EVENT_QUEUE_CAPACITY);
        let sender = EventSender::new(raw_tx);
        for index in 0..(EVENT_QUEUE_CAPACITY - EVENT_QUEUE_CONTROL_RESERVE) {
            sender
                .send(Evt::Log(format!("queued control {index}")))
                .unwrap();
        }

        sender.send_frames(vec![frame(1), frame(1)]);
        assert_eq!(sender.dropped_frames.load(Ordering::Relaxed), 2);
        while rx.try_recv().is_ok() {}
        sender.report_health(3, 4, &CommandHealth::default(), 0);
        let health = rx.try_recv().unwrap();
        assert!(matches!(
            health,
            Evt::CaptureHealth {
                dropped_frames: 2,
                hardware_overruns: 3,
                hardware_errors: 4,
                ..
            }
        ));
    }

    #[test]
    fn command_queue_never_blocks_and_reports_rejection() {
        let (tx, _rx) = bounded(1);
        let health = CommandHealth::default();
        let sender = CommandSender {
            tx,
            health: health.clone(),
        };
        sender.send(Cmd::Start).unwrap();
        assert!(sender.send(Cmd::Stop).is_err());
        assert_eq!(health.rejected.load(Ordering::Relaxed), 1);
        assert_eq!(health.high_watermark.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn critical_command_waits_for_bounded_queue_space() {
        let (tx, rx) = bounded(1);
        let health = CommandHealth::default();
        let sender = CommandSender {
            tx,
            health: health.clone(),
        };
        sender.send(Cmd::Start).unwrap();
        let consumer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            rx.recv().unwrap();
            rx.recv().unwrap();
        });
        sender
            .send_critical(Cmd::Shutdown, Duration::from_millis(250))
            .unwrap();
        consumer.join().unwrap();
        assert_eq!(health.rejected.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn shutdown_signal_bypasses_a_full_command_queue() {
        let (tx, _rx) = bounded(1);
        let health = CommandHealth::default();
        let sender = CommandSender {
            tx,
            health: health.clone(),
        };
        sender.send(Cmd::Start).unwrap();
        assert!(sender.send_critical(Cmd::Shutdown, Duration::ZERO).is_err());
        assert!(health.shutdown_requested.load(Ordering::Acquire));
    }

    #[test]
    #[ignore = "24-hour product gate; run through scripts/run-product-gates.ps1"]
    fn capture_queue_soak_has_no_hidden_loss() {
        let seconds = std::env::var("PCANWORK_SOAK_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(24 * 60 * 60);
        let frames_per_second = std::env::var("PCANWORK_SOAK_FPS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(20_000);
        let batch_interval = Duration::from_millis(10);
        let frames_per_batch = (frames_per_second / 100).max(1) as usize;
        let (raw_tx, rx) = bounded(EVENT_QUEUE_CAPACITY);
        let sender = EventSender::new(raw_tx);
        let received = Arc::new(AtomicU64::new(0));
        let received_worker = received.clone();
        let consumer = std::thread::spawn(move || {
            while let Ok(event) = rx.recv() {
                match event {
                    Evt::Frames(frames) => {
                        received_worker.fetch_add(frames.len() as u64, Ordering::Relaxed);
                    }
                    Evt::Frame(_) => {
                        received_worker.fetch_add(1, Ordering::Relaxed);
                    }
                    _ => {}
                }
            }
        });

        let started = Instant::now();
        let mut sent = 0u64;
        while started.elapsed() < Duration::from_secs(seconds) {
            sender.send_frames(vec![frame(1); frames_per_batch]);
            sent += frames_per_batch as u64;
            std::thread::sleep(batch_interval);
        }
        let dropped = sender.dropped_frames.load(Ordering::Relaxed);
        drop(sender);
        consumer.join().unwrap();

        assert_eq!(
            dropped, 0,
            "capture queue reported {dropped} dropped frames"
        );
        assert_eq!(
            received.load(Ordering::Relaxed),
            sent,
            "capture queue lost frames without an explicit gate failure"
        );
    }

    struct MockAdapter {
        fail_send: bool,
        sent: usize,
    }

    impl CanAdapter for MockAdapter {
        fn poll(&mut self, _out: &mut Vec<CanFrame>) -> PollReport {
            PollReport::default()
        }

        fn send(&mut self, _frame: &CanFrame) -> Result<(), String> {
            self.sent += 1;
            if self.fail_send {
                Err("mock send failure".into())
            } else {
                Ok(())
            }
        }

        fn name(&self) -> &str {
            "mock"
        }
    }

    fn frame(channel: u8) -> CanFrame {
        CanFrame {
            t: 0.0,
            ch: channel,
            tx: false,
            id: 0x123,
            ext: false,
            fd: false,
            brs: false,
            remote: false,
            error: false,
            data: vec![0xFF],
        }
    }

    #[test]
    fn send_rejects_missing_target_channel() {
        let mut adapters: Vec<(u8, Box<dyn CanAdapter>)> = vec![(
            1,
            Box::new(MockAdapter {
                fail_send: false,
                sent: 0,
            }),
        )];

        let error = send_on(&mut adapters, &frame(2)).unwrap_err();
        assert!(error.contains('2'));
    }

    #[test]
    fn pending_send_processing_is_bounded_per_controller_slice() {
        let mut queue = VecDeque::new();
        enqueue_send_job(
            &mut queue,
            PendingSendJob::sequence(frame(1), 100, false, false).unwrap(),
        )
        .unwrap();
        let mut adapters: Vec<(u8, Box<dyn CanAdapter>)> = vec![(
            1,
            Box::new(MockAdapter {
                fail_send: false,
                sent: 0,
            }),
        )];
        let (raw_tx, rx) = bounded(EVENT_QUEUE_CAPACITY);
        let events = EventSender::new(raw_tx);

        process_pending_sends(&mut queue, &mut adapters, &events, Instant::now());

        let emitted = rx
            .try_iter()
            .map(|event| match event {
                Evt::Frames(frames) => frames.len(),
                Evt::Frame(_) => 1,
                _ => 0,
            })
            .sum::<usize>();
        assert!(emitted > 0 && emitted <= SEND_FRAMES_PER_SLICE);
        assert_eq!(pending_send_frames(&queue), 100 - emitted as u64);
    }

    #[test]
    fn online_playback_does_not_emit_frame_after_hardware_failure() {
        let mut adapters: Vec<(u8, Box<dyn CanAdapter>)> = vec![(
            1,
            Box::new(MockAdapter {
                fail_send: true,
                sent: 0,
            }),
        )];
        let (raw_tx, rx) = bounded(8);
        let tx = EventSender::new(raw_tx);

        assert_eq!(
            emit_playback_frame(&mut adapters, &tx, frame(1), true),
            PlaybackFrameEmit::Failed
        );
        let events: Vec<_> = rx.try_iter().collect();
        assert!(events.iter().any(|event| matches!(event, Evt::Log(_))));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Evt::PlaybackFrame(_)))
        );
    }

    #[test]
    fn offline_playback_retries_when_the_ui_queue_is_full() {
        let mut adapters: Vec<(u8, Box<dyn CanAdapter>)> = Vec::new();
        let (raw_tx, rx) = bounded(1);
        let tx = EventSender::new(raw_tx);
        tx.send(Evt::Log("occupy queue".into())).unwrap();

        assert_eq!(
            emit_playback_frame(&mut adapters, &tx, frame(1), false),
            PlaybackFrameEmit::Backpressure
        );
        assert_eq!(tx.dropped_events.load(Ordering::Relaxed), 0);
        assert!(matches!(rx.try_recv().unwrap(), Evt::Log(_)));

        assert_eq!(
            emit_playback_frame(&mut adapters, &tx, frame(1), false),
            PlaybackFrameEmit::Emitted
        );
        assert!(matches!(rx.try_recv().unwrap(), Evt::PlaybackFrame(_)));
    }

    #[test]
    fn playback_completion_waits_for_queue_space() {
        let (raw_tx, rx) = bounded(1);
        let tx = EventSender::new(raw_tx);
        tx.send(Evt::Log("occupy queue".into())).unwrap();
        let consumer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            assert!(matches!(rx.recv().unwrap(), Evt::Log(_)));
            assert!(matches!(rx.recv().unwrap(), Evt::Playback(12, 12, false)));
        });

        tx.send_critical(Evt::Playback(12, 12, false), Duration::from_millis(250))
            .unwrap();
        consumer.join().unwrap();
        assert_eq!(tx.dropped_events.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn ota_ack_requires_the_expected_channel() {
        let ack = OtaAck::XcpAck {
            response: OtaResponseId::Exact(0x123),
        };
        let response = frame(1);

        assert!(!ota_ack_matches_on_channel(ack, 2, 1, &response));
        assert!(ota_ack_matches_on_channel(ack, 2, 2, &response));
    }

    #[test]
    fn simulation_scheduler_atomically_merges_due_signals_and_preserves_others() {
        let text = "VERSION \"\"\nBO_ 256 SimFrame: 2 ECU\n SG_ A : 0|8@1+ (1,0) [0|255] \"\" Vector__XXX\n SG_ B : 8|8@1+ (1,0) [0|255] \"\" Vector__XXX\n";
        let path = std::env::temp_dir().join("pcanwork_sim_atomic_scheduler.dbc");
        std::fs::write(&path, text).unwrap();
        let dbc = DbcDb::load(&path.to_string_lossy()).unwrap();
        let now = Instant::now();
        let mut periodic = SimPeriodic {
            frame: CanFrame {
                t: 0.0,
                ch: 1,
                tx: true,
                id: 0x100,
                ext: false,
                fd: false,
                brs: false,
                remote: false,
                error: false,
                data: vec![0, 0],
            },
            dbc: Some(dbc),
            dbc_id: 0x100,
            generators: vec![
                SimSignalState {
                    config: SimSignalGenerator {
                        signal: "A".into(),
                        mode: SimGeneratorMode::Ramp {
                            min: 10.0,
                            max: 20.0,
                            step: 2.0,
                        },
                        period_ms: 10,
                    },
                    next: now,
                    tick: 0,
                },
                SimSignalState {
                    config: SimSignalGenerator {
                        signal: "B".into(),
                        mode: SimGeneratorMode::Constant { value: 77.0 },
                        period_ms: 100,
                    },
                    next: now,
                    tick: 0,
                },
            ],
            failed: false,
        };

        assert_eq!(update_sim_periodic(&mut periodic, now), Ok(true));
        assert_eq!(periodic.frame.data, [10, 77]);
        assert_eq!(
            update_sim_periodic(&mut periodic, now + Duration::from_millis(10)),
            Ok(true)
        );
        assert_eq!(periodic.frame.data, [12, 77]);
        let _ = std::fs::remove_file(path);
    }
}

fn run_ota_job(
    adapters: &mut Vec<(u8, Box<dyn CanAdapter>)>,
    evt_tx: &EventSender,
    start: Instant,
    buf: &mut Vec<CanFrame>,
    job: OtaJob,
) {
    if adapters.is_empty() {
        let _ = evt_tx.send(Evt::OtaProgress(
            0,
            job.steps.len(),
            "OTA failed: no device connected".into(),
        ));
        return;
    }

    let total = job.steps.len();
    OTA_CANCEL.store(false, Ordering::Relaxed);
    let _ = evt_tx.send(Evt::OtaProgress(0, total, format!("{} started", job.name)));

    for (idx, step) in job.steps.into_iter().enumerate() {
        if OTA_CANCEL.load(Ordering::Relaxed) {
            let _ = evt_tx.send(Evt::OtaProgress(
                idx,
                total,
                format!("{} cancelled", job.name),
            ));
            return;
        }
        let mut ok = false;
        let timeout = Duration::from_millis(step.timeout_ms.max(job.timeout_ms).max(1));
        let retries = step.retries.max(job.retries);
        for attempt in 0..=retries {
            let mut frame = step.frame.clone();
            if OTA_CANCEL.load(Ordering::Relaxed) {
                let _ = evt_tx.send(Evt::OtaProgress(
                    idx,
                    total,
                    format!("{} cancelled", job.name),
                ));
                return;
            }
            frame.t = start.elapsed().as_secs_f64();
            frame.tx = true;
            let expected_channel = frame.ch;
            match send_on(adapters, &frame) {
                Ok(used) => {
                    frame.ch = used;
                    let _ = evt_tx.send(Evt::Frame(frame));
                }
                Err(e) => {
                    let _ = evt_tx.send(Evt::OtaProgress(
                        idx,
                        total,
                        format!("OTA send failed: {e}"),
                    ));
                    return;
                }
            }

            if poll_for_ota_ack(adapters, evt_tx, buf, step.ack, expected_channel, timeout) {
                ok = true;
                break;
            }
            let _ = evt_tx.send(Evt::Log(format!(
                "{} step {}/{} timeout, retry {}/{}",
                job.name,
                idx + 1,
                total,
                attempt + 1,
                retries
            )));
        }

        if !ok {
            let _ = evt_tx.send(Evt::OtaProgress(
                idx,
                total,
                format!("{} failed at step {}", job.name, idx + 1),
            ));
            return;
        }

        let _ = evt_tx.send(Evt::OtaProgress(
            idx + 1,
            total,
            format!("{} {}/{}", job.name, idx + 1, total),
        ));
    }

    let _ = evt_tx.send(Evt::OtaProgress(
        total,
        total,
        format!("{} complete", job.name),
    ));
}

const CONNECTION_LOSS_CONFIRMATIONS: u8 = 3;
const CONNECTION_LOSS_WINDOW: Duration = Duration::from_secs(2);

fn should_log_send_error(
    recent: &mut HashMap<u8, (String, Instant)>,
    channel: u8,
    message: &str,
) -> bool {
    let should_log = recent.get(&channel).is_none_or(|(previous, when)| {
        previous != message || when.elapsed() >= Duration::from_secs(1)
    });
    if should_log {
        recent.insert(channel, (message.to_string(), Instant::now()));
    }
    should_log
}

fn connection_loss_confirmed(
    streaks: &mut HashMap<u8, (u8, Instant)>,
    channel: u8,
    connection_lost: bool,
) -> bool {
    if !connection_lost {
        // Most adapters only perform an expensive health probe periodically;
        // empty receive polls between two probes are not positive proof that
        // the device recovered. Keep the fault streak, but expire it below if
        // the next explicit fault is outside the confirmation window.
        return false;
    }
    let now = Instant::now();
    let streak = streaks.entry(channel).or_insert((0, now));
    if now.duration_since(streak.1) > CONNECTION_LOSS_WINDOW {
        streak.0 = 0;
    }
    streak.0 = streak.0.saturating_add(1);
    streak.1 = now;
    streak.0 >= CONNECTION_LOSS_CONFIRMATIONS
}

#[path = "can/controller.rs"]
mod controller;
use controller::*;
