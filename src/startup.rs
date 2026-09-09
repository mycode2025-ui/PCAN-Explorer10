//! startup responsibilities extracted from src/main.rs.
use super::*;

pub(super) fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    windows_dpi::force_system_dpi_awareness();

    if let Err(error) = license::verify_self_integrity("pcanwork", product_version::current()) {
        rfd::MessageDialog::new()
            .set_title("PcanWork")
            .set_description(format!("程序完整性验证失败，软件无法启动。\n\nApplication integrity verification failed.\n\n{error}"))
            .set_level(rfd::MessageLevel::Error)
            .show();
        return Ok(());
    }

    select_renderer();

    let ui = AppWindow::new()?;
    ui.set_app_version(format!("v{}", product_version::current()).into());
    ui.on_open_website(|| {
        let _ = open_external_url("https://www.hexbyte.cn");
    });
    {
        let weak = ui.as_weak();
        ui.on_check_update(move || start_update_check(weak.clone()));
    }
    {
        let weak = ui.as_weak();
        ui.on_open_update_gitee(move || {
            if let Some(window) = weak.upgrade() {
                let url = window.get_update_gitee_url();
                if !url.is_empty() {
                    let _ = open_external_url(url.as_str());
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_open_update_github(move || {
            if let Some(window) = weak.upgrade() {
                let url = window.get_update_github_url();
                if !url.is_empty() {
                    let _ = open_external_url(url.as_str());
                }
            }
        });
    }
    if !std::env::args().any(|arg| arg.starts_with("--software-stress=")) {
        start_update_check(ui.as_weak());
    }
    ui.set_license_machine_code(license::machine_code().into());
    let trial_duration = license::runtime_trial_duration();
    let license_gate = Rc::new(license::RuntimeGate::new("pcanwork", trial_duration));
    let initially_licensed = license_gate.has_signed_license();
    ui.set_license_unlocked(initially_licensed);
    if let Ok(payload) = license::verify_installed("pcanwork", "*") {
        ui.set_license_info(format!("{} · .pcanlic", payload.license_id).into());
        ui.set_license_validity_zh(license::license_validity(&payload, false).into());
        ui.set_license_validity_en(license::license_validity(&payload, true).into());
    }
    ui.set_license_remaining(
        if initially_licensed {
            "已授权".to_string()
        } else {
            license::format_remaining(trial_duration.as_secs())
        }
        .into(),
    );
    ui.set_license_seconds(trial_duration.as_secs() as i32);
    {
        let weak = ui.as_weak();
        let gate = license_gate.clone();
        ui.on_license_import(move || {
            let Some(window) = weak.upgrade() else { return };
            let Some(path) = rfd::FileDialog::new()
                .add_filter("PcanWork License", &["pcanlic"])
                .set_parent(&window.window().window_handle())
                .pick_file()
            else {
                return;
            };
            match license::install_license(&path, gate.product()) {
                Ok(payload) => {
                    window.set_license_unlocked(true);
                    window.set_license_open(false);
                    window.set_license_remaining(
                        if window.global::<I18n>().get_en() {
                            "Licensed"
                        } else {
                            "已授权"
                        }
                        .into(),
                    );
                    window.set_license_info(format!("{} · .pcanlic", payload.license_id).into());
                    window
                        .set_license_validity_zh(license::license_validity(&payload, false).into());
                    window
                        .set_license_validity_en(license::license_validity(&payload, true).into());
                    window.set_license_error(
                        if window.global::<I18n>().get_en() {
                            "Signed license installed."
                        } else {
                            "签名授权文件已安装。"
                        }
                        .into(),
                    );
                }
                Err(error) => window.set_license_error(
                    if window.global::<I18n>().get_en() {
                        format!("License rejected: {error}")
                    } else {
                        format!("授权文件无效：{error}")
                    }
                    .into(),
                ),
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_license_copy_machine_code(move || {
            let Some(window) = weak.upgrade() else { return };
            match arboard::Clipboard::new().and_then(|mut clipboard| {
                clipboard.set_text(window.get_license_machine_code().to_string())
            }) {
                Ok(()) => window.set_license_error(
                    if window.global::<I18n>().get_en() {
                        "Machine code copied."
                    } else {
                        "机器码已复制。"
                    }
                    .into(),
                ),
                Err(error) => window.set_license_error(
                    if window.global::<I18n>().get_en() {
                        format!("Copy failed: {error}")
                    } else {
                        format!("复制失败：{error}")
                    }
                    .into(),
                ),
            }
        });
    }
    let _license_timer = {
        let weak = ui.as_weak();
        let gate = license_gate.clone();
        let timer = Timer::default();
        timer.start(TimerMode::Repeated, Duration::from_secs(1), move || {
            let Some(window) = weak.upgrade() else { return };
            if gate.has_signed_license() {
                if !window.get_license_unlocked() {
                    window.set_license_unlocked(true);
                    window.set_license_remaining(
                        if window.global::<I18n>().get_en() {
                            "Licensed"
                        } else {
                            "已授权"
                        }
                        .into(),
                    );
                }
                return;
            }
            let remaining = gate.remaining_seconds();
            window.set_license_seconds(remaining.min(i32::MAX as u64) as i32);
            window.set_license_remaining(license::format_remaining(remaining).into());
            if remaining == 0 {
                let _ = window.window().hide();
                let _ = slint::quit_event_loop();
            }
        });
        timer
    };
    let (cmd_tx, evt_rx) = can::spawn();
    let (worker_tx, worker_rx) = crossbeam_channel::bounded::<WorkerEvent>(256);

    let dbc_snap0 = std::sync::Arc::new(ipc::DbcSnapshot::empty());
    let ipc_snapshot =
        std::sync::Arc::new(std::sync::Mutex::new(ipc::Snapshot::new(dbc_snap0.clone())));
    let (ipc_port, ipc_token, ipc_req_rx, ipc_subs) = ipc::spawn_ipc_server(ipc_snapshot.clone());

    let ipc_info_error = std::env::var("PCANWORK_IPC_INFO_FILE")
        .ok()
        .and_then(|info_path| {
            std::fs::write(&info_path, format!("{ipc_port}\n{ipc_token}\n"))
                .err()
                .map(|error| format!("写入 IPC 信息文件失败 {info_path}: {error}"))
        });

    let app = Rc::new(std::cell::RefCell::new(App {
        pending_batches: Vec::new(),
        cmd: cmd_tx.clone(),
        worker_tx,
        license_gate: license_gate.clone(),
        project_name: String::new(),
        project_path: None,
        recent_project_paths: Vec::new(),
        recent_project_model: Rc::new(VecModel::default()),
        sim_dirty: false,
        sim_revision: 0,
        dbcs: Vec::new(),
        mode_trace: true,
        time_mode: 0,
        capture_wall_epoch: None,
        cols_hidden: std::collections::HashSet::new(),
        sim_widgets: Vec::new(),
        sim_tx_frames: HashMap::new(),
        sim_sampler: sim::SimSampler::spawn(),
        sim_sampler_signature: 0,
        sim_sampler_generation: 0,
        sim_sampler_keys: Vec::new(),
        sim_sampler_reported_skips: (0, 0),
        sim_model: Rc::new(VecModel::default()),
        sim_sel: -1,
        sim_multi: std::collections::HashSet::new(),
        sim_running: false,
        sim_canvas_w: 0.0,
        sim_canvas_h: 0.0,
        paused: false,
        autoscroll: true,
        recording: false,
        connected: false,
        connected_channels: std::collections::HashSet::new(),
        shutdown_requested: false,
        conn_name: String::new(),
        running: false,
        baud: "500K".into(),
        device_cfg: DeviceConfig {
            sw_channel: 1,
            is_fd: false,
            device_type: "Virtual".into(),
            hardware_label: String::new(),
            hardware_id: String::new(),
            device_index: 0,
            channel_index: 0,
            baud: "500K".into(),
            data_baud: "2M".into(),
            custom_bitrate: String::new(),
            termination: false,
            listen_only: false,
            fd_non_iso: false,
            net_server: true,
            ip: String::new(),
            port: String::new(),
        },
        channels: vec![DeviceConfig {
            sw_channel: 1,
            is_fd: false,
            device_type: "Virtual".into(),
            hardware_label: String::new(),
            hardware_id: String::new(),
            device_index: 0,
            channel_index: 0,
            baud: "500K".into(),
            data_baud: "2M".into(),
            custom_bitrate: String::new(),
            termination: false,
            listen_only: false,
            fd_non_iso: false,
            net_server: true,
            ip: String::new(),
            port: String::new(),
        }],
        pcan_devices: Vec::new(),
        zcan_devices: Vec::new(),
        last_hardware_scan: None,
        hardware_scan_in_progress: false,
        hardware_scan_status: String::new(),
        channel_edit: None,
        channel_connect_pending: false,
        channel_connect_expected: 0,
        channel_sel: 0,
        recorder: recording::Recorder::spawn(),
        rec_fmt: RecFmt::Csv,
        rec_path: None,
        sig_log: None,
        sig_log_last_flush: None,
        trigger: None,
        dbc_paths: Vec::new(),
        trace: VecDeque::new(),
        no_counter: 0,
        last: HashMap::new(),
        last_dirty: true,
        rx: 0,
        tx: 0,
        err: 0,
        capture_dropped_frames: 0,
        capture_dropped_events: 0,
        capture_hardware_overruns: 0,
        capture_hardware_errors: 0,
        capture_queue_depth: 0,
        capture_queue_capacity: 0,
        capture_queue_high_watermark: 0,
        command_rejected: 0,
        command_queue_depth: 0,
        command_queue_capacity: 0,
        command_queue_high_watermark: 0,
        timestamp_samples: 0,
        timestamp_latest_jitter_us: 0.0,
        timestamp_max_jitter_us: 0.0,
        timestamp_drift_ppm: 0.0,
        timestamp_monotonic_violations: 0,
        series: Vec::new(),
        expr_vars: Vec::new(),
        sig_latest: HashMap::new(),
        expr_decode_ids: HashSet::new(),
        sig_cat: 0,
        signal_pick_expr_selected: None,
        signal_pick_expr_marked: HashSet::new(),
        console_enabled: false,
        console_id: None,
        console_ch: 0,
        console: ConsoleBuf::default(),
        selected_key: None,
        selected_index: -1,
        sig_panel: Vec::new(),
        dbc_signal_choices: Vec::new(),
        filter: Filter::default(),
        txs: Vec::new(),
        tx_sel: -1,
        tx_dbc_order: Vec::new(),
        tx_sig_cache: u64::MAX,
        tx_msgs_cache: u64::MAX,
        tx_list_cache: u64::MAX,
        tx_checked: HashSet::new(),
        tx_speed: 1.0,
        chan_names_cache: u64::MAX,
        next_handle: 1,
        logs: VecDeque::new(),
        sort_col: -1,
        sort_desc: false,
        display_items: Vec::new(),
        expanded_keys: HashSet::new(),
        expanded_signal_cache: HashMap::new(),
        msg_model: Rc::new(VecModel::from(Vec::<MsgRow>::new())),
        chart_model: Rc::new(VecModel::from(Vec::<ChartSeries>::new())),
        chart_xlabel_model: Rc::new(VecModel::default()),
        log_model: Rc::new(VecModel::default()),
        console_model: Rc::new(VecModel::default()),
        dbc_signal_model: Rc::new(VecModel::default()),
        chan_stat_model: Rc::new(VecModel::default()),
        id_stat_model: Rc::new(VecModel::default()),
        sig_model: Rc::new(VecModel::default()),
        dbc_signal_cache: u64::MAX,
        console_cache: u64::MAX,
        sig_panel_cache: u64::MAX,
        trace_cap: TRACE_CAP,
        chart_cap: CHART_CAP,
        tree_collapsed: HashSet::new(),
        tree_row_keys: Vec::new(),
        tree_dbc_index: Vec::new(),
        signal_pick_items: Vec::new(),
        signal_pick_cache: u64::MAX,
        signal_pick_selected: None,
        signal_pick_marked: HashSet::new(),
        signal_pick_msg_expanded: HashSet::new(),
        signal_pick_root_open: true,
        signal_pick_messages_open: true,
        signal_pick_filter: String::new(),
        chart_paused: false,
        chart_normalize: false,
        chart_cursor: false,
        chart_dual: false,
        chart_time_mode: 0,
        chart_y_mode: 0,
        chart_grid: true,
        chart_points: false,
        chart_time_source: 0,
        chart_playback_last_t: None,
        chart_view: None,
        chart_zoom_target: None,
        chart_pause_view: None,
        chart_frozen_series: None,
        chart_highlight: None,
        tree_curve_sig: Vec::new(),
        last_tree_sig: u64::MAX,
        lang_en: false,
        python_interpreter: String::new(),
        last_script_path: String::new(),
        py_child: None,
        py_out_rx: None,
        py_output_dropped: None,
        py_output_dropped_seen: 0,
        py_started: None,
        py_stop_flag: false,
        run_status: String::new(),
        py_output: String::new(),
        py_dirty: false,
        py_timeout_secs: 120,
        ipc_snapshot: ipc_snapshot.clone(),
        ipc_subs: ipc_subs.clone(),
        ipc_handle_map: HashMap::new(),
        dbc_snap: dbc_snap0.clone(),
        pb_raw: Vec::new(),
        pb_files: Vec::new(),
        last_msg_sig: u64::MAX,
        table_cache: Default::default(),
        fps: 0.0,
        bus_load: 0.0,
        win_start: std::time::Instant::now(),
        win_frames: 0,
        win_bits: 0,
        chan_stats: std::collections::BTreeMap::new(),
        pb_pos: 0,
        pb_total: 0,
        pb_playing: false,
    }));
    if let Some(error) = ipc_info_error {
        app.borrow_mut().log(error);
    }

    ui.set_msgs(ModelRc::from(app.borrow().msg_model.clone()));
    ui.set_logs(ModelRc::from(app.borrow().log_model.clone()));
    ui.set_console_lines(ModelRc::from(app.borrow().console_model.clone()));
    ui.set_dbc_signals(ModelRc::from(app.borrow().dbc_signal_model.clone()));
    ui.set_chan_stats(ModelRc::from(app.borrow().chan_stat_model.clone()));
    ui.set_id_stats(ModelRc::from(app.borrow().id_stat_model.clone()));
    ui.set_sigs(ModelRc::from(app.borrow().sig_model.clone()));
    ui.set_recent_projects(ModelRc::from(app.borrow().recent_project_model.clone()));

    ui.set_series(ModelRc::from(app.borrow().chart_model.clone()));
    #[cfg(debug_assertions)]
    if let Some(directory) =
        std::env::args().find_map(|arg| arg.strip_prefix("--software-stress=").map(str::to_owned))
    {
        return software_stress::run(&mut app.borrow_mut(), &ui, std::path::Path::new(&directory));
    }
    let child_windows = ChildWindowStore::default();

    // Main-window callbacks are ready immediately. Secondary windows and their
    // callbacks are constructed together on the first request for a child tool.
    wire_main(app.clone(), &ui, child_windows.clone());
    wire_external_tools(app.clone(), &ui);
    wire_lazy_window_openers(
        app.clone(),
        &ui,
        child_windows.clone(),
        ipc_port,
        ipc_token.clone(),
    );

    {
        let mut a = app.borrow_mut();

        rebuild_dbc_snap(&mut a);

        if let Some(s) = settings::load() {
            a.recent_project_paths = s.recent_project_paths.clone();
            refresh_recent_projects(&a);
            apply_settings(&mut a, &ui, &s);
            sim_migrate_dbc_bindings(&mut a);

            ui.global::<Theme>().set_dark(s.dark);
            ui.global::<Theme>().set_big(s.big);
            ui.global::<I18n>().set_en(s.lang_en);
            a.lang_en = s.lang_en;
            a.log("已恢复上次配置".to_string());
        }

        if let Some(path) = std::env::args_os().skip(1).find_map(|arg| {
            let p = std::path::PathBuf::from(arg);
            let is_project = p
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| {
                    x.eq_ignore_ascii_case("pcprj")
                        || x.eq_ignore_ascii_case("zcp")
                        || x.eq_ignore_ascii_case("json")
                })
                .unwrap_or(false);
            if is_project { Some(p) } else { None }
        }) {
            match std::fs::read_to_string(&path)
                .map_err(|e| format!("Read project failed: {e}"))
                .and_then(|txt| {
                    serde_json::from_str::<Project>(&txt)
                        .map_err(|e| format!("Parse project failed: {e}"))
                }) {
                Ok(proj) => {
                    a.project_name = if proj.name.trim().is_empty() {
                        path.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("CAN_Test_Project")
                            .to_string()
                    } else {
                        proj.name.clone()
                    };
                    a.project_path = Some(path.clone());
                    touch_recent_project(&mut a, &path);
                    refresh_recent_projects(&a);
                    persist_settings(&mut a, &ui);
                    ui.set_project_open(true);
                    a.sim_dirty = false;
                    a.sim_revision = 0;
                    let _ = configure_sim_generators(&a, false);
                    a.sim_running = false;
                    apply_settings(&mut a, &ui, &proj.settings);
                    sim_migrate_dbc_bindings(&mut a);
                    a.txs.clear();
                    for dto in proj.txs {
                        let h = a.next_handle;
                        a.next_handle += 1;
                        a.txs.push(dto.into_task(h));
                    }
                    a.last_tree_sig = u64::MAX;
                    a.log(format!("Opened project: {}", path.display()));
                }
                Err(e) => a.log(e),
            }
        }
    }

    // Bounded receive slices run independently of expensive display refresh.
    let receive_timer = Timer::default();
    {
        let app = app.clone();
        let child_windows = child_windows.clone();
        receive_timer.start(TimerMode::Repeated, CAN_RECEIVE_INTERVAL, move || {
            let windows = child_windows.get();
            let mut a = app.borrow_mut();
            let event_deadline = std::time::Instant::now() + MAX_CAN_EVENT_TIME_PER_TICK;
            for _ in 0..MAX_CAN_EVENTS_PER_TICK {
                if std::time::Instant::now() >= event_deadline {
                    break;
                }
                let Ok(evt) = evt_rx.try_recv() else {
                    break;
                };
                match evt {
                    Evt::Frame(f) => {
                        ipc_fanout(&a, &f);
                        if let Some(windows) = windows.as_ref() {
                            uds_observe_frame(&windows.uds, &f);
                            xcp_observe_frame(&windows.xcp, &f);
                        }
                        a.ingest(f, false);
                    }
                    Evt::Frames(frames) => {
                        for f in &frames {
                            ipc_fanout(&a, f);
                            if let Some(windows) = windows.as_ref() {
                                uds_observe_frame(&windows.uds, f);
                                xcp_observe_frame(&windows.xcp, f);
                            }
                        }
                        a.ingest_batch(frames, false);
                    }
                    Evt::PlaybackFrame(f) => {
                        ipc_fanout(&a, &f);
                        if let Some(windows) = windows.as_ref() {
                            uds_observe_frame(&windows.uds, &f);
                            xcp_observe_frame(&windows.xcp, &f);
                        }
                        a.ingest(f, true);
                    }
                    Evt::PlaybackFrames(frames) => {
                        for f in &frames {
                            ipc_fanout(&a, f);
                            if let Some(windows) = windows.as_ref() {
                                uds_observe_frame(&windows.uds, f);
                                xcp_observe_frame(&windows.xcp, f);
                            }
                        }
                        a.ingest_batch(frames, true);
                    }
                    Evt::Log(s) => a.log(s),
                    Evt::Connected { channels, name, error } => {
                        let attempted_from_config = a.channel_connect_pending;
                        let expected = a.channel_connect_expected;
                        a.connected = !channels.is_empty();
                        a.connected_channels = channels.into_iter().collect();
                        if a.connected && !name.is_empty() {
                            a.conn_name = name.clone();
                            a.log(format!("后端: {name}"));
                        } else if !a.connected {
                            a.conn_name.clear();
                        }
                        if attempted_from_config {
                            a.channel_connect_pending = false;
                            a.channel_connect_expected = 0;
                            if let Some(windows) = windows.as_ref() {
                                windows.channel.set_connecting(false);
                                let success = error.is_none()
                                    && expected > 0
                                    && a.connected_channels.len() == expected;
                                if success {
                                    windows.channel.set_validation_is_error(false);
                                    windows.channel.set_validation_message(
                                        if a.lang_en {
                                            "All channels connected"
                                        } else {
                                            "全部通道连接成功"
                                        }
                                        .into(),
                                    );
                                    a.channel_edit = None;
                                    let _ = windows.channel.hide();
                                } else {
                                    windows.channel.set_validation_is_error(true);
                                    windows.channel.set_validation_message(
                                        error
                                            .unwrap_or_else(|| {
                                                if a.lang_en {
                                                    "Channel connection failed".into()
                                                } else {
                                                    "通道连接失败，请检查设备状态和参数".into()
                                                }
                                            })
                                            .into(),
                                    );
                                }
                            }
                        }
                    }
                    Evt::Running(r) => a.running = r,
                    Evt::Playback(pos, total, playing) => {
                        a.pb_pos = pos;
                        a.pb_total = total;
                        a.pb_playing = playing;
                    }
                    Evt::PeriodicDone(handle) => {
                        if let Some(t) = a.txs.iter_mut().find(|t| t.handle == handle) {
                            t.periodic = false;
                            a.tx_list_cache = u64::MAX;
                        }
                    }
                    Evt::PeriodicProgress { handle, sent } => {
                        if let Some(t) = a.txs.iter_mut().find(|t| t.handle == handle) {
                            t.sent = sent;
                            a.tx_list_cache = u64::MAX;
                        }
                    }
                    Evt::DynamicUpdate {
                        handle,
                        data,
                        signal_values,
                        sent,
                    } => {
                        if let Some(t) = a.txs.iter_mut().find(|t| t.handle == handle) {
                            t.data = data;
                            t.sig_values = signal_values;
                            t.sent = sent;
                            a.tx_list_cache = u64::MAX;
                        }
                    }
                    Evt::CaptureHealth {
                        dropped_frames,
                        dropped_events,
                        hardware_overruns,
                        hardware_errors,
                        queue_depth,
                        queue_capacity,
                        queue_high_watermark,
                        command_rejected,
                        command_queue_depth,
                        command_queue_capacity,
                        command_queue_high_watermark,
                        timestamp_samples,
                        timestamp_latest_jitter_us,
                        timestamp_max_jitter_us,
                        timestamp_drift_ppm,
                        timestamp_monotonic_violations,
                    } => {
                        if dropped_frames > a.capture_dropped_frames {
                            let newly_dropped = dropped_frames - a.capture_dropped_frames;
                            a.log(format!(
                                "严重: CAN UI 队列已丢失 {} 帧（累计 {}），请降低显示负载或停止测量",
                                newly_dropped,
                                dropped_frames
                            ));
                        }
                        if command_rejected > a.command_rejected {
                            let newly_rejected = command_rejected - a.command_rejected;
                            a.log(format!(
                                "严重: CAN 命令队列拒绝了 {} 个操作（累计 {}），操作未执行",
                                newly_rejected, command_rejected
                            ));
                        }
                        a.capture_dropped_frames = dropped_frames;
                        a.capture_dropped_events = dropped_events;
                        a.capture_hardware_overruns = hardware_overruns;
                        a.capture_hardware_errors = hardware_errors;
                        a.capture_queue_depth = queue_depth;
                        a.capture_queue_capacity = queue_capacity;
                        a.capture_queue_high_watermark = queue_high_watermark;
                        a.command_rejected = command_rejected;
                        a.command_queue_depth = command_queue_depth;
                        a.command_queue_capacity = command_queue_capacity;
                        a.command_queue_high_watermark = command_queue_high_watermark;
                        a.timestamp_samples = timestamp_samples;
                        a.timestamp_latest_jitter_us = timestamp_latest_jitter_us;
                        a.timestamp_max_jitter_us = timestamp_max_jitter_us;
                        a.timestamp_drift_ppm = timestamp_drift_ppm;
                        a.timestamp_monotonic_violations = timestamp_monotonic_violations;
                    }
                    Evt::ShutdownFinished => {
                        if let Err(error) = slint::quit_event_loop() {
                            eprintln!("Failed to quit Slint event loop: {error}");
                        }
                    }
                    Evt::OtaProgress(done, total, text) => {
                        let progress = if total == 0 {
                            0.0
                        } else {
                            done as f32 / total as f32
                        };
                        if let Some(windows) = windows.as_ref() {
                            windows.tx.set_tx_file_progress(progress.clamp(0.0, 1.0));
                            windows.tx.set_tx_file_status(text.clone().into());
                            windows.uds.set_ota_status(text.clone().into());
                            windows.xcp.set_ota_status(text.clone().into());
                        }
                        a.log(text);
                    }
                }
            }
            batch_ack::poll(&mut a);
        });
    }

    let timer = Timer::default();
    {
        let app = app.clone();
        let uiw = ui.as_weak();
        let child_windows = child_windows.clone();
        timer.start(TimerMode::Repeated, UI_REFRESH_INTERVAL, move || {
            let windows = child_windows.get();
            let playback_window = child_windows.get_playback();
            {
                let mut a = app.borrow_mut();
                while let Some(event) = a.recorder.try_event() {
                    match event {
                        recording::Event::Started { path, format } => {
                            a.recording = true;
                            a.rec_fmt = format;
                            a.rec_path = Some(path.clone());
                            a.log(format!("开始记录({}): {}", format.name(), path.display()));
                        }
                        recording::Event::Stopped {
                            path,
                            format,
                            frames,
                        } => {
                            a.recording = false;
                            a.log(format!(
                                "已保存 {}: {}（{} 帧）",
                                format.name(),
                                path.display(),
                                frames
                            ));
                        }
                        recording::Event::Failed(error) => {
                            a.recording = false;
                            a.log(format!("记录失败并已停止: {error}"));
                        }
                    }
                }
                while let Ok(event) = worker_rx.try_recv() {
                    match event {
                        WorkerEvent::Log(message) => a.log(message),
                        WorkerEvent::PlaybackParsed {
                            replace,
                            files,
                            errors,
                        } => {
                            if replace {
                                a.pb_files.clear();
                            }
                            let frame_count: usize =
                                files.iter().map(|(_, frames)| frames.len()).sum();
                            a.pb_files.extend(files);
                            for error in errors {
                                a.log(error);
                            }
                            let file_count = a.pb_files.len();
                            a.log(format!(
                                "已载入 {file_count} 个回放文件，本次 {frame_count} 帧"
                            ));
                            if let Some(window) = playback_window.as_ref() {
                                pb_apply_files(&mut a, window);
                            }
                        }
                        WorkerEvent::ConversionFinished { batch, status, log } => {
                            if let Some(windows) = windows.as_ref() {
                                if batch {
                                    windows.convert.set_status2(status.into());
                                } else {
                                    windows.convert.set_status1(status.into());
                                }
                            }
                            a.log(log);
                        }
                        WorkerEvent::DbcLoaded { path, result } => match result {
                            Ok(db) => {
                                let count = db.messages().count();
                                a.log(format!("已加载 DBC: {} ({count} 条报文)", db.file_name));
                                a.dbcs.push(db);
                                a.dbc_paths.push(path);
                                a.expanded_signal_cache.clear();
                                rebuild_dbc_snap(&mut a);
                            }
                            Err(error) => a.log(format!("加载 DBC 失败: {error}")),
                        },
                        WorkerEvent::DbcReloaded { loaded, errors } => {
                            a.dbcs.clear();
                            a.dbc_paths.clear();
                            a.expanded_signal_cache.clear();
                            for (path, db) in loaded {
                                a.dbc_paths.push(path);
                                a.dbcs.push(db);
                            }
                            for error in errors {
                                a.log(error);
                            }
                            rebuild_dbc_snap(&mut a);
                            let count = a.dbcs.len();
                            a.log(format!("已重新加载 {count} 个 DBC"));
                        }
                        WorkerEvent::ProjectLoaded { path, result } => match *result {
                            Ok((mut project, loaded_dbcs, errors, replace_dbcs)) => {
                                let Some(main_window) = uiw.upgrade() else {
                                    continue;
                                };
                                let dark = project.settings.dark;
                                let big = project.settings.big;
                                let english = project.settings.lang_en;
                                a.lang_en = english;
                                project.settings.dbc_path = None;
                                project.settings.dbc_paths.clear();
                                let tasks = a.txs.clone();
                                for task in &tasks {
                                    stop_task_periodic(&a, task);
                                }
                                a.project_name = if project.name.trim().is_empty() {
                                    path.file_stem()
                                        .and_then(|name| name.to_str())
                                        .unwrap_or("CAN_Test_Project")
                                        .to_string()
                                } else {
                                    project.name
                                };
                                a.project_path = Some(path.clone());
                                touch_recent_project(&mut a, &path);
                                refresh_recent_projects(&a);
                                persist_settings(&mut a, &main_window);
                                main_window.set_project_open(true);
                                a.sim_dirty = false;
                                a.sim_revision = 0;
                                let _ = configure_sim_generators(&a, false);
                                a.sim_running = false;
                                a.sim_sel = -1;
                                a.sim_multi.clear();
                                apply_settings(&mut a, &main_window, &project.settings);
                                if replace_dbcs {
                                    a.dbcs.clear();
                                    a.dbc_paths.clear();
                                    a.expanded_signal_cache.clear();
                                    for (dbc_path, database) in loaded_dbcs {
                                        a.dbc_paths.push(dbc_path);
                                        a.dbcs.push(database);
                                    }
                                    rebuild_dbc_snap(&mut a);
                                }
                                sim_migrate_dbc_bindings(&mut a);
                                a.txs.clear();
                                let count = project.txs.len();
                                for dto in project.txs {
                                    let handle = a.next_handle;
                                    a.next_handle += 1;
                                    a.txs.push(dto.into_task(handle));
                                }
                                for error in errors {
                                    a.log(error);
                                }
                                a.last_tree_sig = u64::MAX;
                                a.log(format!(
                                    "已打开工程: {}（发送任务 {count} 条，默认停发）",
                                    path.display()
                                ));

                                main_window.global::<Theme>().set_dark(dark);
                                main_window.global::<Theme>().set_big(big);
                                main_window.global::<I18n>().set_en(english);
                                if let Some(windows) = windows.as_ref() {
                                    windows.set_dark(dark);
                                    windows.set_big(big);
                                    windows.set_language(english);
                                }
                            }
                            Err(error) => a.log(error),
                        },
                        WorkerEvent::ProjectSaved {
                            path,
                            sim_revision,
                            result,
                        } => match result {
                            Ok(()) => {
                                a.project_path = Some(path.clone());
                                touch_recent_project(&mut a, &path);
                                refresh_recent_projects(&a);
                                if let Some(main_window) = uiw.upgrade() {
                                    persist_settings(&mut a, &main_window);
                                }
                                if a.sim_revision == sim_revision {
                                    a.sim_dirty = false;
                                }
                                a.log(format!("已保存工程: {}", path.display()));
                            }
                            Err(error) => a.log(error),
                        },
                        WorkerEvent::TxFilePrepared {
                            path,
                            repeat,
                            english,
                            result,
                        } => {
                            if let Some(windows) = windows.as_ref() {
                                let window = &windows.tx;
                                match result {
                                    Ok(TxFilePayload::Ota(job)) => {
                                        let total = job.steps.len();
                                        if !a.license_allows("firmware-update") {
                                            window.set_tx_file_status(
                                                if english {
                                                    "License required"
                                                } else {
                                                    "需要有效授权"
                                                }
                                                .into(),
                                            );
                                            continue;
                                        }
                                        if a.cmd.send(Cmd::OtaRun(job)).is_err() {
                                            window.set_tx_file_status(
                                                if english {
                                                    "CAN backend has stopped"
                                                } else {
                                                    "CAN 后台线程已退出"
                                                }
                                                .into(),
                                            );
                                        } else {
                                            window.set_tx_file_progress(0.0);
                                            window.set_tx_file_status(
                                                if english {
                                                    format!("OTA started ({total} steps)")
                                                } else {
                                                    format!("OTA 已启动（{total} 步）")
                                                }
                                                .into(),
                                            );
                                        }
                                    }
                                    Ok(TxFilePayload::Frames(frames)) => {
                                        let target = batch_ack::Target::File {
                                            window: window.as_weak(),
                                            path,
                                            english,
                                        };
                                        match batch_ack::submit(&mut a, frames, repeat, target) {
                                            Ok(()) => window.set_tx_file_status(
                                                if english {
                                                    "Waiting for backend confirmation…"
                                                } else {
                                                    "等待后台确认…"
                                                }
                                                .into(),
                                            ),
                                            Err(error) => window.set_tx_file_status(error.into()),
                                        }
                                    }
                                    Err(error) => window.set_tx_file_status(error.into()),
                                }
                            }
                        }
                        WorkerEvent::TxListLoaded(result) => match result {
                            Ok(dtos) => {
                                let tasks = a.txs.clone();
                                for task in &tasks {
                                    stop_task_periodic(&a, task);
                                }
                                a.txs.clear();
                                let count = dtos.len();
                                for dto in dtos {
                                    let handle = a.next_handle;
                                    a.next_handle += 1;
                                    a.txs.push(dto.into_task(handle));
                                }
                                a.log(format!("已加载发送列表 {count} 条（默认停发）"));
                            }
                            Err(error) => a.log(format!("加载发送列表失败: {error}")),
                        },
                        WorkerEvent::HardwareScanned {
                            pcan,
                            zcan,
                            elapsed_ms,
                        } => {
                            a.pcan_devices = pcan;
                            a.zcan_devices = zcan;
                            a.hardware_scan_in_progress = false;
                            a.hardware_scan_status =
                                if a.pcan_devices.is_empty() && a.zcan_devices.is_empty() {
                                    if a.lang_en {
                                        format!("No hardware found · {elapsed_ms} ms")
                                    } else {
                                        format!("未发现硬件 · {elapsed_ms} ms")
                                    }
                                } else if a.lang_en {
                                    format!(
                                        "{} channel(s) detected · {elapsed_ms} ms",
                                        a.pcan_devices.len() + a.zcan_devices.len()
                                    )
                                } else {
                                    format!(
                                        "已发现 {} 个物理通道 · {elapsed_ms} ms",
                                        a.pcan_devices.len() + a.zcan_devices.len()
                                    )
                                };
                            reconcile_stable_hardware(&mut a);
                            if let Some(windows) = windows.as_ref() {
                                refresh_channel_window_lists(&windows.channel, &a);
                                let selected = channel_selected(&a);
                                if let Some(channel) = channel_configs(&a).get(selected as usize) {
                                    set_chan_form(&windows.channel, channel, &a);
                                }
                            }
                        }
                    }
                }
                for _ in 0..64 {
                    let Ok(ureq) = ipc_req_rx.try_recv() else {
                        break;
                    };
                    handle_ipc(&mut a, ureq);
                }
                reap_child(&mut a);
                drain_py_output(&mut a);
                publish_snapshot(&mut a);

                if a.py_dirty {
                    if a.py_child.is_none() {
                        let path = run_log_path();
                        if let Err(error) = std::fs::write(&path, &a.py_output) {
                            a.log(format!("保存测试运行日志失败 {}: {error}", path.display()));
                            if !a.run_status.starts_with("FAIL") {
                                a.run_status = "FAIL: 运行日志保存失败".into();
                            }
                        }
                    }
                    if let Some(windows) = windows.as_ref() {
                        let w = &windows.script_runner;
                        w.set_output(a.py_output.clone().into());
                        w.set_running(a.py_child.is_some());
                        let rs = a.run_status.clone();
                        w.set_result(if rs.starts_with("PASS") {
                            1
                        } else if rs.starts_with("FAIL") {
                            -1
                        } else {
                            0
                        });
                        w.set_status_text(rs.into());
                    }
                    a.py_dirty = false;
                }
            }
            let ui = match uiw.upgrade() {
                Some(u) => u,
                None => return,
            };
            let mut a = app.borrow_mut();

            let dt = a.win_start.elapsed().as_secs_f64();
            if dt >= 1.0 {
                a.fps = a.win_frames as f64 / dt;
                let default_bps = baud_bps(&a.device_cfg.baud);

                let bps_of: std::collections::HashMap<u8, f64> = a
                    .channels
                    .iter()
                    .map(|c| (c.sw_channel, baud_bps(&c.baud)))
                    .collect();
                let mut max_load = 0.0_f64;
                for (ch, cs) in a.chan_stats.iter_mut() {
                    cs.fps = cs.win_frames as f64 / dt;
                    let bps = bps_of.get(ch).copied().unwrap_or(default_bps);
                    cs.bus_load = if bps > 0.0 {
                        (cs.win_bits as f64 / dt / bps * 100.0).min(100.0)
                    } else {
                        0.0
                    };
                    if cs.bus_load > max_load {
                        max_load = cs.bus_load;
                    }
                    cs.win_frames = 0;
                    cs.win_bits = 0;
                }
                a.bus_load = max_load;
                a.win_frames = 0;
                a.win_bits = 0;
                a.win_start = std::time::Instant::now();
            }

            sim_tick(&mut a);
            refresh_sim(&a);
            if let Some(windows) = windows.as_ref() {
                refresh_sim_context(&windows.sim_panel, &a);
            }

            batch_ack::poll(&mut a);
            refresh_ui(&mut a, &ui, windows.as_deref());
        });
    }

    let pb_timer = Timer::default();
    {
        let app = app.clone();
        let child_windows = child_windows.clone();
        pb_timer.start(TimerMode::Repeated, Duration::from_millis(150), move || {
            let Some(w) = child_windows.get_playback() else {
                return;
            };
            let a = app.borrow();
            let en = a.lang_en;
            w.set_pos(a.pb_pos.to_string().into());
            w.set_total(a.pb_total.to_string().into());
            w.set_playing(a.pb_playing);
            w.set_status(
                if a.pb_total == 0 {
                    if en {
                        "No file loaded"
                    } else {
                        "未载入文件"
                    }
                } else if a.pb_playing {
                    if en { "Playing" } else { "回放中" }
                } else if a.pb_pos >= a.pb_total {
                    if en { "Done" } else { "回放完成" }
                } else {
                    if en {
                        "Ready / Paused"
                    } else {
                        "就绪/已暂停"
                    }
                }
                .into(),
            );
        });
    }

    #[cfg(windows)]
    let _titlebar_timer = {
        let t = slint::Timer::default();
        t.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(500),
            apply_brand_titlebar,
        );
        apply_brand_titlebar();
        t
    };

    #[cfg(debug_assertions)]
    if std::env::var_os("PCANWORK_DEBUG_OPEN_SIM").is_some() {
        let ui = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(250), move || {
            if let Some(ui) = ui.upgrade() {
                ui.invoke_open_sim_panel_window();
            }
        });
    }

    #[cfg(debug_assertions)]
    if std::env::var_os("PCANWORK_DEBUG_TEST_SIM_LIBRARY").is_some() {
        let ui = ui.as_weak();
        let app = app.clone();
        let child_windows = child_windows.clone();
        slint::Timer::single_shot(std::time::Duration::from_millis(300), move || {
            let Some(ui) = ui.upgrade() else { return };
            ui.invoke_open_sim_panel_window();
            let app = app.clone();
            let child_windows = child_windows.clone();
            slint::Timer::single_shot(std::time::Duration::from_millis(450), move || {
                let Some(windows) = child_windows.get() else {
                    panic!("simulation child windows were not created");
                };
                let panel = &windows.sim_panel;
                panel.invoke_signal_library_refresh();
                panel.invoke_signal_library_row_clicked(0, false);
                let rows = panel.get_signal_library_rows();
                let message_index = (0..rows.row_count())
                    .find(|index| {
                        rows.row_data(*index)
                            .is_some_and(|row| row.kind == "message")
                    })
                    .expect("DBC signal library did not expose a message")
                    as i32;
                panel.invoke_signal_library_row_clicked(message_index, false);
                let rows = panel.get_signal_library_rows();
                let signal_index = (0..rows.row_count())
                    .find(|index| {
                        rows.row_data(*index)
                            .is_some_and(|row| row.kind == "signal")
                    })
                    .expect("DBC signal library did not expose a signal")
                    as i32;
                panel.invoke_signal_library_activate(signal_index);
                {
                    let app = app.borrow();
                    assert!(
                        app.sim_widgets
                            .iter()
                            .any(|widget| !widget.signal.is_empty()),
                        "DBC signal library activation did not bind or create a control"
                    );
                }
                let before_drop = app.borrow().sim_widgets.len();
                panel.invoke_signal_library_drop(signal_index, 900.0, 240.0);
                assert_eq!(
                    app.borrow().sim_widgets.len(),
                    before_drop + 1,
                    "dropping a DBC signal on blank canvas did not create a control"
                );
                let (target_x, target_y, before_rebind) = {
                    let app = app.borrow();
                    let widget = &app.sim_widgets[0];
                    (
                        (widget.x + widget.w / 2.0) as f32,
                        (widget.y + widget.h / 2.0) as f32,
                        app.sim_widgets.len(),
                    )
                };
                panel.invoke_signal_library_drop(signal_index, target_x, target_y);
                assert_eq!(
                    app.borrow().sim_widgets.len(),
                    before_rebind,
                    "dropping on an existing control unexpectedly created another control"
                );
                panel.invoke_signal_library_create(-1, SimKind::Indicator.to_i32());
                assert!(
                    app.borrow().sim_widgets.len() > before_rebind,
                    "batch create from marked signals did not create a control"
                );
            });
        });
    }

    ui.run()?;
    Ok(())
}
