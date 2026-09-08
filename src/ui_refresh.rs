//! Main-window model synchronization and refresh diagnostics.
use super::*;
use crate::chart::advance_chart_zoom;

pub(super) fn refresh_ui(a: &mut App, ui: &AppWindow, child_windows: Option<&ChildWindows>) {
    advance_chart_zoom(a);
    let started = std::time::Instant::now();
    let validation = validate_filter(&ui.get_f_id(), &ui.get_f_data());
    ui.set_filter_invalid(validation.is_err());
    let mut draft = parse_filter(&ui.get_f_id(), &ui.get_f_name(), &ui.get_f_data());
    draft.dir_filter = dir_idx_to_opt(ui.get_dir_filter());
    ui.set_filter_feedback(
        match validation {
            Err(error) => error,
            Ok(()) if draft != a.filter => if a.lang_en {
                "Pending changes — click Apply"
            } else {
                "待应用：点击应用筛选"
            }
            .into(),
            Ok(()) if a.filter == Filter::default() => if a.lang_en {
                "No active filter"
            } else {
                "未启用筛选"
            }
            .into(),
            Ok(()) => if a.lang_en {
                "Filter applied"
            } else {
                "筛选已生效"
            }
            .into(),
        }
        .into(),
    );
    ui.set_connected(a.connected);
    ui.set_running(a.running);
    ui.set_recording(a.recording);
    ui.set_mode_trace(a.mode_trace);
    ui.set_paused(a.paused);
    ui.set_auto_scroll(a.autoscroll);
    ui.set_rx_count(a.rx.to_string().into());
    ui.set_tx_count(a.tx.to_string().into());
    ui.set_err_count(a.err.to_string().into());
    ui.set_capture_health(
        format!(
            "UI 当前/峰值 {:.1}/{:.1} ms · 接收队列 {queue} · 丢帧 {drops}\n表格 当前/峰值 {:.1}/{:.1} ms · 更新行 {}\n接收队列 {}/{} · 峰值 {} · 丢帧 {}\n命令队列 {}/{} · 峰值 {} · 拒绝 {}\n记录队列 {}/{} · 峰值 {} · 丢帧 {}\n硬件溢出 {} · 错误 {}\n时间戳样本 {} · 抖动 {:.0}/{:.0} us · 漂移 {:+.1} ppm · 非单调 {}",
            a.table_cache.ui_ms,
            a.table_cache.ui_peak_ms,
            a.table_cache.refresh_ms,
            a.table_cache.peak_ms,
            a.table_cache.updated_rows,
            a.capture_queue_depth,
            a.capture_queue_capacity,
            a.capture_queue_high_watermark,
            a.capture_dropped_frames,
            a.command_queue_depth,
            a.command_queue_capacity,
            a.command_queue_high_watermark,
            a.command_rejected,
            a.recorder.queue_depth(),
            a.recorder.queue_capacity(),
            a.recorder.queue_high_watermark(),
            a.recorder.dropped_frames(),
            a.capture_hardware_overruns,
            a.capture_hardware_errors,
            a.timestamp_samples,
            a.timestamp_latest_jitter_us,
            a.timestamp_max_jitter_us,
            a.timestamp_drift_ppm,
            a.timestamp_monotonic_violations,
            queue = a.capture_queue_depth,
            drops = a.capture_dropped_frames,
        )
        .into(),
    );
    ui.set_capture_loss(
        a.capture_dropped_frames > 0
            || a.capture_dropped_events > 0
            || a.capture_hardware_overruns > 0
            || a.capture_hardware_errors > 0
            || a.command_rejected > 0
            || a.timestamp_monotonic_violations > 0
            || a.recorder.dropped_frames() > 0,
    );
    ui.set_fps(format!("{:.0}", a.fps).into());
    ui.set_bus_load(format!("{:.1}%", a.bus_load).into());
    ui.set_load_high(a.bus_load >= 70.0);

    let chan_load = if a.chan_stats.len() >= 2 {
        a.chan_stats
            .iter()
            .map(|(ch, cs)| format!("CAN{ch} {:.0}%", cs.bus_load))
            .collect::<Vec<_>>()
            .join("  ")
    } else {
        String::new()
    };
    ui.set_chan_load(chan_load.into());
    ui.set_baud(a.baud.clone().into());
    ui.set_total_count(a.trace.len().to_string().into());
    let sel_id_txt = a
        .selected_key
        .map(|k| {
            let id = (k & 0xFFFF_FFFF) as u32;
            let ext = ((k >> 38) & 1) == 1;
            let nm = a.dbc_message_name_frame(id, ext).unwrap_or("");
            if nm.is_empty() {
                format!("0x{id:X}")
            } else {
                format!("0x{id:X} {nm}")
            }
        })
        .unwrap_or_else(|| "无".into());
    ui.set_sel_id(sel_id_txt.into());

    ui.set_selected(a.selected_index);

    build_msg_table(a, ui);

    build_signal_panel(a, ui);

    let dbc_signal_sig = std::sync::Arc::as_ptr(&a.dbc_snap) as usize as u64;
    if dbc_signal_sig != a.dbc_signal_cache {
        a.dbc_signal_cache = dbc_signal_sig;
        a.dbc_signal_choices.clear();
        let mut dbc_signal_rows: Vec<SharedString> = Vec::new();
        let mut choices: Vec<(u32, String, String, String)> = Vec::new();
        let mut seen: std::collections::HashSet<(u32, String)> = std::collections::HashSet::new();
        for d in &a.dbcs {
            for m in d.messages() {
                for s in &m.signals {
                    if seen.insert((m.id, s.name.clone())) {
                        choices.push((m.id, m.name.clone(), s.name.clone(), s.unit.clone()));
                    }
                }
            }
        }
        choices.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.cmp(&b.2)));
        for (id, msg_name, sig_name, unit) in choices {
            a.dbc_signal_choices.push((id, sig_name.clone()));
            let unit_suffix = if unit.is_empty() {
                String::new()
            } else {
                format!(" [{unit}]")
            };
            dbc_signal_rows.push(format!("0x{id:X} {msg_name} / {sig_name}{unit_suffix}").into());
        }
        if dbc_signal_rows.is_empty() {
            dbc_signal_rows.push("(无 DBC 信号)".into());
        }
        sync_vec_model(&a.dbc_signal_model, dbc_signal_rows);
    }
    if let Some(windows) = child_windows {
        refresh_signal_picker(a, &windows.signal);

        refresh_chart(a, ui, &windows.chart);

        {
            let sig = tx_list_sig(a);
            if sig != a.tx_list_cache {
                a.tx_list_cache = sig;
                push_tx_list(a, ui, &windows.tx);
            }
        }

        let chan_names: Vec<SharedString> = a
            .channels
            .iter()
            .map(|c| format!("CAN{}", c.sw_channel).into())
            .collect();
        {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            chan_names.hash(&mut h);
            let sig = h.finish();
            if sig != a.chan_names_cache {
                a.chan_names_cache = sig;
                windows
                    .tx
                    .set_channel_names(ModelRc::from(Rc::new(VecModel::from(chan_names))));
            }
        }

        build_tx_dbc_page(a, &windows.tx);
    }

    build_stats(a, ui);

    tree::build_tree(a, ui);

    if a.console_cache != a.console.revision {
        a.console_cache = a.console.revision;
        let console_rows = a.console.rows().into_iter().map(Into::into).collect();
        sync_vec_model(&a.console_model, console_rows);
    }
    a.table_cache.ui_ms = started.elapsed().as_secs_f64() * 1000.0;
    a.table_cache.ui_peak_ms = a.table_cache.ui_peak_ms.max(a.table_cache.ui_ms);
}
