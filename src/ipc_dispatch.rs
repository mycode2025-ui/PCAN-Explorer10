//! ipc dispatch responsibilities extracted from src/main.rs.
use super::*;

pub(super) fn publish_snapshot(a: &mut App) {
    let last_rebuild = if a.last_dirty {
        let mut last = HashMap::with_capacity(a.last.len());
        for (k, li) in a.last.iter() {
            last.insert(
                *k,
                ipc::LastSnap {
                    t: li.t,
                    count: li.count,
                    data: li.data.clone(),
                    ext: li.ext,
                },
            );
        }
        a.last_dirty = false;
        Some(last)
    } else {
        None
    };
    if let Ok(mut snap) = a.ipc_snapshot.lock() {
        snap.connected = a.connected;
        snap.running = a.running;
        snap.rx = a.rx;
        snap.tx = a.tx;
        snap.err = a.err;
        snap.no_counter = a.no_counter;
        snap.bus_load = a.bus_load;
        snap.fps = a.fps;
        snap.dropped_frames = a.capture_dropped_frames;
        snap.dropped_events = a.capture_dropped_events;
        snap.hardware_overruns = a.capture_hardware_overruns;
        snap.hardware_errors = a.capture_hardware_errors;
        snap.event_queue_depth = a.capture_queue_depth;
        snap.event_queue_capacity = a.capture_queue_capacity;
        snap.event_queue_high_watermark = a.capture_queue_high_watermark;
        snap.command_rejected = a.command_rejected;
        snap.command_queue_depth = a.command_queue_depth;
        snap.command_queue_capacity = a.command_queue_capacity;
        snap.command_queue_high_watermark = a.command_queue_high_watermark;
        snap.timestamp_samples = a.timestamp_samples;
        snap.timestamp_latest_jitter_us = a.timestamp_latest_jitter_us;
        snap.timestamp_max_jitter_us = a.timestamp_max_jitter_us;
        snap.timestamp_drift_ppm = a.timestamp_drift_ppm;
        snap.timestamp_monotonic_violations = a.timestamp_monotonic_violations;
        snap.last_log = a.logs.back().cloned().unwrap_or_default();
        snap.recent_logs = a.logs.iter().rev().take(100).cloned().collect();
        snap.recent_logs.reverse();
        snap.channels = if a.connected {
            let mut channels: Vec<u8> = a.connected_channels.iter().copied().collect();
            channels.sort_unstable();
            channels
                .into_iter()
                .map(|ch| {
                    let cs = a.chan_stats.get(&ch).cloned().unwrap_or_default();
                    ipc::ChanStatSnap {
                        ch,
                        rx: cs.rx,
                        tx: cs.tx,
                        err: cs.err,
                        bus_load: cs.bus_load,
                        fps: cs.fps,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        snap.console_enabled = a.console_enabled;
        if a.console_enabled {
            snap.console_text = a.console.export_text();
        }
        if let Some(last) = last_rebuild {
            snap.last = last;
        }
        snap.dbc = a.dbc_snap.clone();
    }
}

pub(super) fn rebuild_dbc_snap(a: &mut App) {
    a.dbc_snap = std::sync::Arc::new(ipc::DbcSnapshot::from_dbcs(&a.dbcs));

    recompute_expr_ids(a);
}

pub(super) fn stop_internal_periodic(a: &App, internal: u64) {
    let dummy = CanFrame {
        t: 0.0,
        ch: 1,
        tx: true,
        id: 0,
        ext: false,
        fd: false,
        brs: false,
        remote: false,
        error: false,
        data: Vec::new(),
    };
    let _ = a.cmd.send(Cmd::SetPeriodic {
        handle: internal,
        frame: dummy,
        period_ms: 1,
        repeat: -1,
        enable: false,
    });
}

pub(super) fn handle_ipc(a: &mut App, ureq: ipc::UiReq) {
    use ipc::{IpcReq, IpcResp};
    let cid = ureq.client_id;
    let ok = || IpcResp::Ok(serde_json::json!({}));
    let license_denied = || IpcResp::Err {
        code: "LICENSE_REQUIRED".into(),
        msg: "试用已结束，需要有效的 .pcanlic 授权".into(),
    };

    let mut periodic_rollback: Option<u64> = None;
    let resp = match ureq.req {
        IpcReq::Invalid { code, msg } => IpcResp::Err { code, msg },
        IpcReq::SendOnce {
            ch,
            id,
            data,
            ext,
            fd,
            brs,
            remote,
        } => {
            if !a.license_allows("can-transmit") {
                license_denied()
            } else {
                match validate_ipc_tx_frame(ch, id, data, ext, fd, brs, remote) {
                    Ok(frame) => match a.cmd.send(Cmd::SendOnce(frame)) {
                        Ok(()) => ok(),
                        Err(_) => IpcResp::Err {
                            code: "BUSY".into(),
                            msg: "CAN 命令队列已满，本帧未提交，请稍后重试".into(),
                        },
                    },
                    Err(msg) => IpcResp::Err {
                        code: "BAD_FRAME".into(),
                        msg,
                    },
                }
            }
        }
        IpcReq::SendBatch { frames, repeat } => {
            if !a.license_allows("can-transmit") {
                license_denied()
            } else if frames.is_empty() {
                IpcResp::Err {
                    code: "BAD_ARG".into(),
                    msg: "frames 不能为空".into(),
                }
            } else if repeat == 0 {
                IpcResp::Err {
                    code: "BAD_ARG".into(),
                    msg: "repeat 必须大于 0".into(),
                }
            } else {
                let total = (frames.len() as u64).saturating_mul(repeat as u64);
                if total > 100_000 {
                    IpcResp::Err {
                        code: "BATCH_LIMIT".into(),
                        msg: format!("批量发送共 {total} 帧，超过 100000 帧安全上限"),
                    }
                } else {
                    let mut validated = Vec::with_capacity(frames.len());
                    let mut error = None;
                    for (index, frame) in frames.into_iter().enumerate() {
                        match validate_ipc_tx_frame(
                            frame.ch,
                            frame.id,
                            frame.data,
                            frame.ext,
                            frame.fd,
                            frame.brs,
                            frame.remote,
                        ) {
                            Ok(frame) => validated.push(frame),
                            Err(message) => {
                                error = Some(format!("frames[{index}]: {message}"));
                                break;
                            }
                        }
                    }
                    if let Some(msg) = error {
                        IpcResp::Err {
                            code: "BAD_FRAME".into(),
                            msg,
                        }
                    } else {
                        match batch_ack::submit(
                            a,
                            validated,
                            repeat,
                            batch_ack::Target::Ipc(ureq.reply.clone()),
                        ) {
                            Ok(()) => return,
                            Err(msg) => IpcResp::Err {
                                code: "BUSY".into(),
                                msg,
                            },
                        }
                    }
                }
            }
        }
        IpcReq::SetPeriodic {
            client_handle,
            ch,
            id,
            data,
            period_ms,
            repeat,
            ext,
            fd,
            brs,
            remote,
        } => {
            if !a.license_allows("can-transmit") {
                license_denied()
            } else {
                match validate_ipc_tx_frame(ch, id, data, ext, fd, brs, remote) {
                    Err(msg) => IpcResp::Err {
                        code: "BAD_FRAME".into(),
                        msg,
                    },
                    Ok(frame) => {
                        let internal = a.next_handle | (1u64 << 63);
                        a.next_handle += 1;
                        if let Some(old) = a.ipc_handle_map.insert((cid, client_handle), internal) {
                            stop_internal_periodic(a, old);
                        }
                        match a.cmd.send(Cmd::SetPeriodic {
                            handle: internal,
                            frame,
                            period_ms: period_ms.max(1),
                            repeat,
                            enable: true,
                        }) {
                            Ok(()) => {
                                periodic_rollback = Some(internal);
                                ok()
                            }
                            Err(_) => {
                                a.ipc_handle_map.remove(&(cid, client_handle));
                                IpcResp::Err {
                                    code: "BUSY".into(),
                                    msg: "CAN 命令队列已满，周期任务未提交，请稍后重试".into(),
                                }
                            }
                        }
                    }
                }
            }
        }
        IpcReq::StopPeriodic { client_handle } => {
            if let Some(internal) = a.ipc_handle_map.remove(&(cid, client_handle)) {
                stop_internal_periodic(a, internal);
            }
            ok()
        }
        IpcReq::Connect { channels } => {
            if !a.license_allows("can-connect") {
                license_denied()
            } else if channels.is_empty() {
                IpcResp::Err {
                    code: "BAD_ARG".into(),
                    msg: "channels 不能为空(至少给一个通道配置)".into(),
                }
            } else {
                a.channels = channels.clone();
                let _ = a.cmd.send(Cmd::ConnectChannels(channels));
                ok()
            }
        }
        IpcReq::ConnectConfigured => {
            if !a.license_allows("can-connect") {
                license_denied()
            } else {
                let _ = a.cmd.send(Cmd::ConnectChannels(a.channels.clone()));
                let expected_channels: Vec<u8> = a
                    .channels
                    .iter()
                    .map(|channel| channel.sw_channel.max(1))
                    .collect();
                IpcResp::Ok(serde_json::json!({
                    "channels": a.channels.len(),
                    "expected_channels": expected_channels,
                }))
            }
        }
        IpcReq::LoadDbc { path, loaded } => {
            if a.dbc_paths.iter().any(|x| x == &path) {
                IpcResp::Ok(serde_json::json!({ "loaded": false, "name": path, "note": "已加载" }))
            } else {
                match loaded {
                    Ok(db) => {
                        let name = db.file_name.clone();
                        a.dbcs.push(db);
                        a.dbc_paths.push(path.clone());
                        rebuild_dbc_snap(a);
                        a.log(format!("脚本加载 DBC: {name}"));
                        IpcResp::Ok(serde_json::json!({ "loaded": true, "name": name }))
                    }
                    Err(e) => IpcResp::Err {
                        code: "LOAD_FAIL".into(),
                        msg: e,
                    },
                }
            }
        }
        IpcReq::Disconnect => {
            let _ = a.cmd.send(Cmd::Disconnect);
            ok()
        }
        IpcReq::Start => {
            if !a.license_allows("can-capture") {
                license_denied()
            } else {
                let _ = a.cmd.send(Cmd::Start);
                ok()
            }
        }
        IpcReq::Stop => {
            let _ = a.cmd.send(Cmd::Stop);
            ok()
        }
        IpcReq::Log { msg } => {
            a.log(msg);
            ok()
        }
        IpcReq::RunResult { passed, summary } => {
            a.run_status = format!("{} {summary}", if passed { "PASS" } else { "FAIL" });
            let rs = a.run_status.clone();
            a.log(format!("[脚本] {rs}"));
            ok()
        }
        IpcReq::ConsoleSet {
            enabled,
            id,
            ch,
            clear,
        } => {
            if let Some(en) = enabled {
                a.console_enabled = en;
            }
            if let Some(idv) = id {
                a.console_id = if idv < 0 { None } else { Some(idv as u32) };
            }
            if let Some(c) = ch {
                a.console_ch = c;
            }
            if clear {
                a.console.clear();
            }
            ok()
        }
        IpcReq::ClientGone => {
            let internals: Vec<u64> = a
                .ipc_handle_map
                .iter()
                .filter(|((c, _), _)| *c == cid)
                .map(|(_, h)| *h)
                .collect();
            for internal in internals {
                stop_internal_periodic(a, internal);
            }
            a.ipc_handle_map.retain(|(c, _), _| *c != cid);
            ok()
        }
    };

    if ureq.reply.try_send(resp).is_err()
        && let Some(internal) = periodic_rollback
    {
        stop_internal_periodic(a, internal);
        a.ipc_handle_map.retain(|_, h| *h != internal);
    }
}

pub(super) fn validate_ipc_tx_frame(
    ch: u8,
    id: u32,
    data: Vec<u8>,
    ext: bool,
    fd: bool,
    brs: bool,
    remote: bool,
) -> Result<CanFrame, String> {
    if ch == 0 {
        return Err("CAN 通道必须从 1 开始".into());
    }
    let max_id = if ext { 0x1FFF_FFFF } else { 0x7FF };
    if id > max_id {
        return Err(format!(
            "ID 0x{id:X} 超出{}帧范围 0x0..0x{max_id:X}",
            if ext { "扩展" } else { "标准" }
        ));
    }
    if brs && !fd {
        return Err("BRS 只能用于 CAN FD 帧".into());
    }
    if remote && fd {
        return Err("CAN FD 不支持远程帧".into());
    }
    if remote && !data.is_empty() {
        return Err("远程帧不能携带数据字节".into());
    }
    if !fd && data.len() > 8 {
        return Err(format!(
            "经典 CAN 最多 8 字节，当前为 {} 字节；需要显式设置 fd=True",
            data.len()
        ));
    }
    if fd && !matches!(data.len(), 0..=8 | 12 | 16 | 20 | 24 | 32 | 48 | 64) {
        return Err(format!(
            "CAN FD 数据长度 {} 无法直接映射 DLC；允许 0..8、12、16、20、24、32、48、64 字节",
            data.len()
        ));
    }
    Ok(CanFrame {
        t: 0.0,
        ch,
        tx: true,
        id,
        ext,
        fd,
        brs,
        remote,
        error: false,
        data,
    })
}

pub(super) fn ipc_fanout(a: &App, f: &CanFrame) {
    let subs = a.ipc_subs.subs.lock().unwrap();
    if subs.is_empty() {
        return;
    }
    let line = ipc::frame_event_json(f);
    for s in subs.iter() {
        if !s.ids.is_empty() && !s.ids.contains(&f.id) {
            continue;
        }
        if s.out.try_send(line.clone()).is_err() {
            s.dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}
