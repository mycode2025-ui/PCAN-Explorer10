//! controller responsibilities extracted from src/can.rs.
use super::*;

pub(super) fn controller(
    cmd_rx: EventReceiver<Cmd>,
    evt_tx: EventSender,
    command_health: CommandHealth,
) {
    let start = Instant::now();
    let mut last_health_report = Instant::now();
    let mut hardware_overruns = 0u64;
    let mut hardware_errors = 0u64;
    let mut last_adapter_error: HashMap<u8, (String, Instant)> = HashMap::new();
    let mut last_send_error: HashMap<u8, (String, Instant)> = HashMap::new();
    let mut connection_loss_streak: HashMap<u8, (u8, Instant)> = HashMap::new();
    let mut adapters: Vec<(u8, Box<dyn CanAdapter>)> = Vec::new();
    let mut running = false;
    let mut periodics: HashMap<u64, Periodic> = HashMap::new();
    let mut dynamic_periodics: HashMap<u64, DynamicPeriodic> = HashMap::new();
    let mut sim_periodics: Vec<SimPeriodic> = Vec::new();
    let mut pending_sends: VecDeque<PendingSendJob> = VecDeque::new();
    let mut buf: Vec<CanFrame> = Vec::with_capacity(1024);
    let mut playback: Option<Playback> = None;

    loop {
        if command_health.shutdown_requested.load(Ordering::Acquire) {
            pending_sends.clear();
            adapters.clear();
            clear_vci_device_registry();
            periodics.clear();
            dynamic_periodics.clear();
            sim_periodics.clear();
            let _ = evt_tx.send_critical(Evt::ShutdownFinished, Duration::from_secs(1));
            return;
        }
        for _ in 0..64 {
            if command_health.shutdown_requested.load(Ordering::Acquire) {
                break;
            }
            match cmd_rx.try_recv() {
                Ok(cmd) => match cmd {
                    Cmd::Connect => {
                        evt_tx.begin_timestamp_session();
                        pending_sends.clear();
                        hardware_overruns = 0;
                        hardware_errors = 0;
                        last_adapter_error.clear();
                        connection_loss_streak.clear();
                        match PcanBus::open(start) {
                            Ok(p) => {
                                let n = p.name().to_string();
                                adapters = vec![(1, Box::new(p))];
                                let _ = evt_tx.send(Evt::Log(format!("已连接真实 PCAN 卡: {n}")));
                                let _ = evt_tx.send(Evt::Connected {
                                    channels: vec![1],
                                    name: n,
                                    error: None,
                                });
                            }
                            Err(e) => {
                                let _ = evt_tx.send(Evt::Log(format!("连接 PCAN 卡失败: {e}")));
                                let _ = evt_tx.send(Evt::Connected {
                                    channels: Vec::new(),
                                    name: String::new(),
                                    error: Some(e),
                                });
                            }
                        }
                    }
                    Cmd::ConnectConfig(cfg) => {
                        evt_tx.begin_timestamp_session();
                        pending_sends.clear();
                        hardware_overruns = 0;
                        hardware_errors = 0;
                        last_adapter_error.clear();
                        connection_loss_streak.clear();
                        dynamic_periodics.clear();
                        connect_channels(
                            &mut adapters,
                            &mut running,
                            &mut periodics,
                            &evt_tx,
                            start,
                            vec![cfg],
                        );
                    }
                    Cmd::ConnectChannels(cfgs) => {
                        evt_tx.begin_timestamp_session();
                        pending_sends.clear();
                        hardware_overruns = 0;
                        hardware_errors = 0;
                        last_adapter_error.clear();
                        connection_loss_streak.clear();
                        dynamic_periodics.clear();
                        connect_channels(
                            &mut adapters,
                            &mut running,
                            &mut periodics,
                            &evt_tx,
                            start,
                            cfgs,
                        );
                    }
                    Cmd::Disconnect => {
                        pending_sends.clear();
                        adapters.clear();
                        running = false;
                        periodics.clear();
                        dynamic_periodics.clear();
                        connection_loss_streak.clear();
                        let _ = evt_tx.send(Evt::Running(false));
                        let _ = evt_tx.send(Evt::Connected {
                            channels: Vec::new(),
                            name: String::new(),
                            error: None,
                        });
                        let _ = evt_tx.send(Evt::Log("已断开设备".into()));
                    }
                    Cmd::Start => {
                        if !adapters.is_empty() {
                            running = true;
                            let _ = evt_tx.send(Evt::Running(true));
                            let _ = evt_tx.send(Evt::Log("启动接收".into()));
                        } else {
                            let _ = evt_tx.send(Evt::Log("未连接设备，无法启动".into()));
                        }
                    }
                    Cmd::Stop => {
                        if !pending_sends.is_empty() {
                            pending_sends.clear();
                            let _ = evt_tx.send(Evt::Log("已取消待发送任务".into()));
                        }
                        running = false;
                        let _ = evt_tx.send(Evt::Running(false));
                        let _ = evt_tx.send(Evt::Log("停止接收".into()));
                    }
                    Cmd::SendOnce(mut f) => {
                        if !adapters.is_empty() {
                            f.t = start.elapsed().as_secs_f64();
                            match send_on(&mut adapters, &f) {
                                Ok(used) => {
                                    let mut echo = f.clone();
                                    echo.tx = true;
                                    echo.ch = used;
                                    let _ = evt_tx.send(Evt::Frame(echo));
                                }
                                Err(e) => {
                                    let _ = evt_tx.send(Evt::Log(format!("发送失败: {e}")));
                                }
                            }
                        }
                    }
                    Cmd::SendSequence {
                        frame,
                        count,
                        id_increment,
                        data_increment,
                    } => {
                        if adapters.is_empty() {
                            let _ =
                                evt_tx.send(Evt::Log("发送失败: 当前没有已连接的 CAN 通道".into()));
                        } else if let Some(job) =
                            PendingSendJob::sequence(frame, count, id_increment, data_increment)
                        {
                            match enqueue_send_job(&mut pending_sends, job) {
                                Ok(queued) => {
                                    let _ = evt_tx
                                        .send(Evt::Log(format!("已加入发送队列: {queued} 帧")));
                                }
                                Err(error) => {
                                    let _ = evt_tx
                                        .send(Evt::Log(format!("发送任务未加入队列: {error}")));
                                }
                            }
                        }
                    }
                    Cmd::SendBatch {
                        frames,
                        repeat,
                        ack,
                    } => {
                        let result = if adapters.is_empty() {
                            Err("当前没有已连接的 CAN 通道".to_string())
                        } else if let Some(job) = PendingSendJob::batch(frames, repeat.max(1)) {
                            enqueue_send_job(&mut pending_sends, job).map_err(str::to_string)
                        } else {
                            Err("批量发送列表不能为空".to_string())
                        };
                        match &result {
                            Ok(queued) => {
                                let _ = evt_tx
                                    .send(Evt::Log(format!("批量发送已加入队列: {queued} 帧")));
                            }
                            Err(error) => {
                                let _ = evt_tx
                                    .send(Evt::Log(format!("批量发送任务未加入队列: {error}")));
                            }
                        }
                        if let Some(ack) = ack {
                            let _ = ack.send(result);
                        }
                    }
                    Cmd::OtaRun(job) => {
                        run_ota_job(&mut adapters, &evt_tx, start, &mut buf, job);
                    }
                    Cmd::SetPeriodic {
                        handle,
                        frame,
                        period_ms,
                        repeat,
                        enable,
                    } => {
                        if enable && repeat != 0 {
                            dynamic_periodics.remove(&handle);
                            periodics.insert(
                                handle,
                                Periodic {
                                    frame,
                                    period: Duration::from_millis(period_ms.max(1)),
                                    next: Instant::now(),
                                    remaining: repeat,
                                    sent: 0,
                                },
                            );
                        } else {
                            periodics.remove(&handle);
                        }
                    }
                    Cmd::SetDynamicPeriodic { handle, config } => {
                        periodics.remove(&handle);
                        if let Some(config) = config.filter(|cfg| cfg.repeat != 0) {
                            dynamic_periodics.insert(
                                handle,
                                DynamicPeriodic {
                                    sent: config.start_sent,
                                    config,
                                    next: Instant::now(),
                                },
                            );
                        } else {
                            dynamic_periodics.remove(&handle);
                        }
                    }
                    Cmd::SetSimulationPeriodics(configs) => {
                        let now = Instant::now();
                        sim_periodics = configs
                            .into_iter()
                            .map(|config| SimPeriodic {
                                frame: config.frame,
                                dbc: config.dbc,
                                dbc_id: config.dbc_id,
                                generators: config
                                    .generators
                                    .into_iter()
                                    .map(|config| SimSignalState {
                                        config,
                                        next: now,
                                        tick: 0,
                                    })
                                    .collect(),
                                failed: false,
                            })
                            .collect();
                    }
                    Cmd::PlaybackLoad(frames) => {
                        let total = frames.len();
                        playback = Some(Playback {
                            frames,
                            idx: 0,
                            online: false,
                            speed: 1.0,
                            playing: false,
                            paused: false,
                            base: Instant::now(),
                            base_t: 0.0,
                            loop_play: false,
                        });
                        let _ = evt_tx.send(Evt::Log(format!("已载入回放文件，共 {total} 帧")));
                        let _ = evt_tx.send_critical(
                            Evt::Playback(0, total, false),
                            Duration::from_millis(250),
                        );
                    }
                    Cmd::PlaybackPlay {
                        online,
                        speed,
                        loop_play,
                    } => {
                        if let Some(pb) = playback.as_mut() {
                            if pb.idx >= pb.frames.len() {
                                pb.idx = 0;
                            }
                            pb.online = online;
                            pb.speed = speed;
                            pb.loop_play = loop_play;
                            pb.playing = true;
                            pb.paused = false;
                            pb.base = Instant::now();
                            pb.base_t = pb.frames.get(pb.idx).map(|f| f.t).unwrap_or(0.0);
                            let _ = evt_tx.send(Evt::Log(format!(
                                "开始回放（{}，{}）",
                                if online { "在线" } else { "离线" },
                                if speed <= 0.0 {
                                    "尽可能快".to_string()
                                } else {
                                    format!("{speed}x")
                                }
                            )));
                        }
                    }
                    Cmd::PlaybackPause => {
                        if let Some(pb) = playback.as_mut() {
                            pb.paused = true;
                            let _ = evt_tx.send_critical(
                                Evt::Playback(pb.idx, pb.frames.len(), false),
                                Duration::from_millis(250),
                            );
                        }
                    }
                    Cmd::PlaybackStep => {
                        if let Some(pb) = playback.as_mut() {
                            pb.playing = false;
                            if pb.idx < pb.frames.len() {
                                let t0 = pb.frames[pb.idx].t;
                                if !pb.online {
                                    let mut end = pb.idx;
                                    while end < pb.frames.len() && pb.frames[end].t < t0 + 0.1 {
                                        end += 1;
                                    }
                                    let frames = pb.frames[pb.idx..end].to_vec();
                                    if matches!(
                                        evt_tx.try_send_playback_frames(frames),
                                        PlaybackEventSend::Enqueued
                                    ) {
                                        pb.idx = end;
                                    }
                                } else {
                                    while pb.idx < pb.frames.len() && pb.frames[pb.idx].t < t0 + 0.1
                                    {
                                        let frame = pb.frames[pb.idx].clone();
                                        match emit_playback_frame(
                                            &mut adapters,
                                            &evt_tx,
                                            frame,
                                            true,
                                        ) {
                                            PlaybackFrameEmit::Emitted => pb.idx += 1,
                                            PlaybackFrameEmit::Backpressure => break,
                                            PlaybackFrameEmit::Failed => {
                                                pb.playing = false;
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                            let _ = evt_tx.send_critical(
                                Evt::Playback(pb.idx, pb.frames.len(), false),
                                Duration::from_millis(250),
                            );
                        }
                    }
                    Cmd::PlaybackCancel => {
                        if let Some(pb) = playback.as_mut() {
                            pb.idx = 0;
                            pb.playing = false;
                            pb.paused = false;
                            let _ = evt_tx.send_critical(
                                Evt::Playback(0, pb.frames.len(), false),
                                Duration::from_millis(250),
                            );
                            let _ = evt_tx.send(Evt::Log("已取消回放".into()));
                        }
                    }
                    Cmd::PlaybackSeek(frac) => {
                        if let Some(pb) = playback.as_mut()
                            && !pb.frames.is_empty()
                        {
                            let i = ((frac.clamp(0.0, 1.0) * pb.frames.len() as f64) as usize)
                                .min(pb.frames.len() - 1);
                            pb.idx = i;
                            pb.base = Instant::now();
                            pb.base_t = pb.frames[i].t;
                            let _ = evt_tx.send_critical(
                                Evt::Playback(pb.idx, pb.frames.len(), pb.playing),
                                Duration::from_millis(250),
                            );
                        }
                    }
                    Cmd::Shutdown => {
                        pending_sends.clear();
                        adapters.clear();
                        clear_vci_device_registry();
                        periodics.clear();
                        dynamic_periodics.clear();
                        sim_periodics.clear();
                        let _ = evt_tx.send_critical(Evt::ShutdownFinished, Duration::from_secs(1));
                        return;
                    }
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    adapters.clear();
                    clear_vci_device_registry();
                    return;
                }
            }
        }

        if command_health.shutdown_requested.load(Ordering::Acquire) {
            continue;
        }

        process_pending_sends(&mut pending_sends, &mut adapters, &evt_tx, start);

        if !periodics.is_empty() && !adapters.is_empty() {
            let now = Instant::now();
            let mut due: Vec<(u64, CanFrame)> = Vec::new();
            let mut done: Vec<u64> = Vec::new();
            for (h, p) in periodics.iter_mut() {
                if now >= p.next {
                    p.next = now + p.period;
                    let mut f = p.frame.clone();
                    f.t = start.elapsed().as_secs_f64();
                    f.tx = true;
                    due.push((*h, f));
                }
            }
            let mut sent_frames = Vec::with_capacity(due.len());
            for (handle, mut f) in due {
                match send_on(&mut adapters, &f) {
                    Ok(used) => {
                        f.ch = used;
                        sent_frames.push(f);
                        if let Some(periodic) = periodics.get_mut(&handle) {
                            periodic.sent += 1;
                            if periodic.remaining > 0 {
                                periodic.remaining -= 1;
                                if periodic.remaining == 0 {
                                    done.push(handle);
                                }
                            }
                            let _ = evt_tx.send(Evt::PeriodicProgress {
                                handle,
                                sent: periodic.sent,
                            });
                        }
                    }
                    Err(error) => {
                        if should_log_send_error(&mut last_send_error, f.ch, &error) {
                            let _ = evt_tx.send(Evt::Log(format!("周期发送失败: {error}")));
                        }
                    }
                }
            }
            if !sent_frames.is_empty() {
                let _ = evt_tx.send(Evt::Frames(sent_frames));
            }
            for h in done {
                periodics.remove(&h);
                let _ = evt_tx.send(Evt::PeriodicDone(h));
            }
        }

        if !dynamic_periodics.is_empty() && !adapters.is_empty() {
            let now = Instant::now();
            let mut due = Vec::new();
            let mut done = Vec::new();
            for (handle, periodic) in dynamic_periodics.iter_mut() {
                if now < periodic.next {
                    continue;
                }
                periodic.next += Duration::from_millis(periodic.config.period_ms.max(1));
                if periodic.next <= now {
                    periodic.next = now + Duration::from_millis(periodic.config.period_ms.max(1));
                }
                match build_dynamic_frame(*handle, periodic) {
                    Ok((frame, signal_values)) => {
                        due.push((*handle, frame, signal_values, periodic.sent + 1))
                    }
                    Err(error) => {
                        let _ = evt_tx.send(Evt::Log(format!("动态周期发送编码失败: {error}")));
                        done.push(*handle);
                    }
                }
                periodic.sent += 1;
                if periodic.config.repeat > 0 && periodic.sent >= periodic.config.repeat as u64 {
                    done.push(*handle);
                }
            }
            for (handle, mut frame, signal_values, sent) in due {
                frame.t = start.elapsed().as_secs_f64();
                frame.tx = true;
                match send_on(&mut adapters, &frame) {
                    Ok(channel) => {
                        frame.ch = channel;
                        let _ = evt_tx.send(Evt::DynamicUpdate {
                            handle,
                            data: frame.data.clone(),
                            signal_values,
                            sent,
                        });
                        let _ = evt_tx.send(Evt::Frame(frame));
                    }
                    Err(error) => {
                        if should_log_send_error(&mut last_send_error, frame.ch, &error) {
                            let _ = evt_tx.send(Evt::Log(format!("动态周期发送失败: {error}")));
                        }
                        done.push(handle);
                    }
                }
            }
            done.sort_unstable();
            done.dedup();
            for handle in done {
                dynamic_periodics.remove(&handle);
                let _ = evt_tx.send(Evt::PeriodicDone(handle));
            }
        }

        if !sim_periodics.is_empty() && !adapters.is_empty() {
            let now = Instant::now();
            let mut due = Vec::new();
            for periodic in &mut sim_periodics {
                match update_sim_periodic(periodic, now) {
                    Ok(true) => {
                        periodic.failed = false;
                        let mut frame = periodic.frame.clone();
                        frame.t = start.elapsed().as_secs_f64();
                        frame.tx = true;
                        due.push(frame);
                    }
                    Ok(false) => {}
                    Err(error) => {
                        if !periodic.failed {
                            let _ = evt_tx.send(Evt::Log(format!(
                                "仿真发生器编码失败: CAN{} 0x{:X}: {error}",
                                periodic.frame.ch, periodic.frame.id
                            )));
                        }
                        periodic.failed = true;
                    }
                }
            }
            for mut frame in due {
                match send_on(&mut adapters, &frame) {
                    Ok(channel) => {
                        frame.ch = channel;
                        let _ = evt_tx.send(Evt::Frame(frame));
                    }
                    Err(error) => {
                        let _ = evt_tx.send(Evt::Log(format!(
                            "仿真发生器发送失败: CAN{} 0x{:X}: {error}",
                            frame.ch, frame.id
                        )));
                    }
                }
            }
        }

        let mut pb_active = false;
        if let Some(pb) = playback.as_mut()
            && pb.playing
            && !pb.paused
        {
            pb_active = true;
            if pb.speed <= 0.0 {
                if !pb.online {
                    let end = (pb.idx + 500).min(pb.frames.len());
                    let frames = pb.frames[pb.idx..end].to_vec();
                    match evt_tx.try_send_playback_frames(frames) {
                        PlaybackEventSend::Enqueued => pb.idx = end,
                        PlaybackEventSend::Full => {}
                        PlaybackEventSend::Disconnected => pb.playing = false,
                    }
                } else {
                    for _ in 0..500 {
                        if pb.idx >= pb.frames.len() {
                            break;
                        }
                        let frame = pb.frames[pb.idx].clone();
                        match emit_playback_frame(&mut adapters, &evt_tx, frame, true) {
                            PlaybackFrameEmit::Emitted => pb.idx += 1,
                            PlaybackFrameEmit::Backpressure => break,
                            PlaybackFrameEmit::Failed => {
                                pb.playing = false;
                                break;
                            }
                        }
                    }
                }
            } else {
                let now = Instant::now();
                if !pb.online {
                    let mut end = pb.idx;
                    while end < pb.frames.len() && end - pb.idx < 500 {
                        let dt = (pb.frames[end].t - pb.base_t).max(0.0) / pb.speed;
                        if now < pb.base + Duration::from_secs_f64(dt) {
                            break;
                        }
                        end += 1;
                    }
                    if end > pb.idx {
                        let frames = pb.frames[pb.idx..end].to_vec();
                        match evt_tx.try_send_playback_frames(frames) {
                            PlaybackEventSend::Enqueued => pb.idx = end,
                            PlaybackEventSend::Full => {}
                            PlaybackEventSend::Disconnected => pb.playing = false,
                        }
                    }
                } else {
                    loop {
                        if pb.idx >= pb.frames.len() {
                            break;
                        }
                        let dt = (pb.frames[pb.idx].t - pb.base_t).max(0.0) / pb.speed;
                        if now >= pb.base + Duration::from_secs_f64(dt) {
                            let frame = pb.frames[pb.idx].clone();
                            match emit_playback_frame(&mut adapters, &evt_tx, frame, true) {
                                PlaybackFrameEmit::Emitted => pb.idx += 1,
                                PlaybackFrameEmit::Backpressure => break,
                                PlaybackFrameEmit::Failed => {
                                    pb.playing = false;
                                    break;
                                }
                            }
                        } else {
                            break;
                        }
                    }
                }
            }
            if pb.idx >= pb.frames.len() {
                if pb.loop_play && !pb.frames.is_empty() {
                    pb.idx = 0;
                    pb.base = Instant::now();
                    pb.base_t = pb.frames[0].t;
                    let _ = evt_tx.send(Evt::Playback(0, pb.frames.len(), true));
                } else {
                    pb.playing = false;
                    // Completion must not be lost behind the playback-frame burst. The UI
                    // relies on this state to fill the progress bar and leave "回放中".
                    let _ = evt_tx.send_critical(
                        Evt::Playback(pb.idx, pb.frames.len(), false),
                        Duration::from_secs(2),
                    );
                    let _ = evt_tx
                        .send_critical(Evt::Log("回放完成".into()), Duration::from_millis(250));
                }
            } else {
                let _ = evt_tx.send(Evt::Playback(pb.idx, pb.frames.len(), pb.playing));
            }
        }

        if running {
            let mut fatal_disconnect: Option<(u8, String)> = None;
            for (ch, a) in adapters.iter_mut() {
                buf.clear();
                let report = a.poll(&mut buf);
                hardware_overruns = hardware_overruns.saturating_add(report.receive_overruns);
                hardware_errors = hardware_errors.saturating_add(report.driver_errors);
                let connection_lost = report.connection_lost;
                let loss_reason = report
                    .message
                    .clone()
                    .unwrap_or_else(|| "设备连接已丢失".into());
                if let Some(message) = report.message {
                    let should_log = last_adapter_error.get(ch).is_none_or(|(previous, when)| {
                        previous != &message || when.elapsed() >= Duration::from_secs(1)
                    });
                    if should_log {
                        let _ = evt_tx.send(Evt::Log(format!("CAN{ch}: {message}")));
                        last_adapter_error.insert(*ch, (message, Instant::now()));
                    }
                }
                if fatal_disconnect.is_none()
                    && connection_loss_confirmed(&mut connection_loss_streak, *ch, connection_lost)
                {
                    fatal_disconnect = Some((*ch, loss_reason));
                }
                for f in &mut buf {
                    f.ch = *ch;
                }
                for batch in buf.chunks(128) {
                    evt_tx.send_frames(batch.to_vec());
                }
            }
            if let Some((channel, reason)) = fatal_disconnect {
                pending_sends.clear();
                adapters.clear();
                clear_vci_device_registry();
                running = false;
                periodics.clear();
                dynamic_periodics.clear();
                connection_loss_streak.clear();
                let _ = evt_tx.send(Evt::Running(false));
                let _ = evt_tx.send(Evt::Connected {
                    channels: Vec::new(),
                    name: String::new(),
                    error: Some(reason.clone()),
                });
                let _ = evt_tx.send(Evt::Log(format!(
                    "CAN{channel} 设备连接已丢失，采集已安全停止: {reason}"
                )));
            }
            if last_health_report.elapsed() >= Duration::from_millis(250) {
                evt_tx.report_health(
                    hardware_overruns,
                    hardware_errors,
                    &command_health,
                    cmd_rx.len(),
                );
                last_health_report = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(1));
        } else if pb_active {
            std::thread::sleep(Duration::from_millis(1));
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
