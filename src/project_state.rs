//! project state responsibilities extracted from src/main.rs.
use super::*;

pub(super) fn gather_settings(a: &App, ui: &AppWindow) -> settings::Settings {
    let th = ui.global::<Theme>();
    settings::Settings {
        channels: a.channels.clone(),
        channel_sel: a.channel_sel,
        dark: th.get_dark(),
        big: th.get_big(),
        trace_cap: a.trace_cap,
        chart_cap: a.chart_cap,
        f_id: ui.get_f_id().to_string(),
        f_name: ui.get_f_name().to_string(),
        f_data: ui.get_f_data().to_string(),
        dir_filter: ui.get_dir_filter(),
        log_error_frames: a.log_error_frames,
        log_error_counter_changes: a.log_error_counter_changes,
        dbc_path: None,
        dbc_paths: a.dbc_paths.clone(),
        left_w: ui.get_left_w(),
        bottom_h: ui.get_bottom_h(),
        mode_trace: a.mode_trace,
        time_mode: a.time_mode,
        cols_hidden: {
            let mut v: Vec<&str> = a.cols_hidden.iter().map(|s| s.as_str()).collect();
            v.sort_unstable();
            v.join(",")
        },
        sim_widgets: serde_json::to_string(&a.sim_widgets).unwrap_or_default(),
        lang_en: ui.global::<I18n>().get_en(),
        python_interpreter_path: a.python_interpreter.clone(),
        last_script_path: a.last_script_path.clone(),
        expr_vars: a.expr_vars.clone(),
        console_enabled: a.console_enabled,
        console_id: a.console_id.map(|x| x as i64).unwrap_or(-1),
        console_ch: a.console_ch as i32,
        renderer: ui.get_renderer_mode().to_string(),
        recent_project_paths: a.recent_project_paths.clone(),
    }
}

pub(super) fn persist_settings(a: &mut App, ui: &AppWindow) {
    let result = settings::save(&gather_settings(a, ui));
    if let Err(error) = result {
        a.log(format!("保存最近工程失败: {error}"));
    }
}

pub(super) fn persist_project_if_open(a: &mut App, ui: &AppWindow) {
    persist_settings(a, ui);
    let Some(path) = a.project_path.clone() else {
        return;
    };
    let project = Project {
        name: a.project_name.clone(),
        settings: gather_settings(a, ui),
        txs: a.txs.iter().map(TxTaskDto::from_task).collect(),
    };
    let worker = a.worker_tx.clone();
    let sim_revision = a.sim_revision;
    std::thread::spawn(move || {
        let result = serde_json::to_string_pretty(&project)
            .map_err(|error| format!("序列化工程失败: {error}"))
            .and_then(|text| {
                std::fs::write(&path, text).map_err(|error| format!("保存工程失败: {error}"))
            });
        let _ = worker.send(WorkerEvent::ProjectSaved {
            path,
            sim_revision,
            result,
        });
    });
}

pub(super) fn commit_channel_edit(a: &mut App) -> Result<usize, String> {
    ensure_channel_edit_session(a);
    let session = a.channel_edit.as_ref().expect("channel edit session");
    can::validate_channel_set(&session.channels)?;
    a.channels = session.channels.clone();
    a.channel_sel = session
        .selected
        .clamp(0, a.channels.len() as i32 - 1)
        .max(0);
    if let Some(first) = a.channels.first().cloned() {
        a.baud = first.baud.clone();
        a.device_cfg = first;
    }
    if let Some(session) = a.channel_edit.as_mut() {
        session.channels = a.channels.clone();
        session.selected = a.channel_sel;
        session.dirty = false;
    }
    Ok(a.channels.len())
}

pub(super) fn touch_recent_project(a: &mut App, path: &std::path::Path) {
    let path = path.to_string_lossy().to_string();
    a.recent_project_paths
        .retain(|item| !item.eq_ignore_ascii_case(&path));
    a.recent_project_paths.insert(0, path);
    a.recent_project_paths.truncate(12);
}

pub(super) fn refresh_recent_projects(a: &App) {
    let rows = a
        .recent_project_paths
        .iter()
        .map(|path| {
            let file = std::path::Path::new(path);
            let available = file.is_file();
            let name = file
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or(path);
            let modified = std::fs::metadata(file)
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .map(|time| {
                    chrono::DateTime::<chrono::Local>::from(time)
                        .format("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_default();
            RecentProjectRow {
                name: name.into(),
                path: path.as_str().into(),
                modified: modified.into(),
                available,
            }
        })
        .collect::<Vec<_>>();
    sync_vec_model(&a.recent_project_model, rows);
}

pub(super) fn apply_settings(a: &mut App, ui: &AppWindow, s: &settings::Settings) {
    if !s.channels.is_empty() {
        a.channels = s.channels.clone();
        a.channel_sel = s.channel_sel.clamp(0, a.channels.len() as i32 - 1).max(0);
    }
    a.python_interpreter = s.python_interpreter_path.clone();
    a.last_script_path = s.last_script_path.clone();
    a.expr_vars = s.expr_vars.clone();
    let rmode = if s.renderer.is_empty() {
        "auto".to_string()
    } else {
        s.renderer.clone()
    };
    ui.set_renderer_mode(rmode.into());
    recompute_expr_ids(a);

    a.console_enabled = s.console_enabled;
    a.console_id = if s.console_id < 0 {
        None
    } else {
        Some(s.console_id as u32)
    };
    a.console_ch = s.console_ch.clamp(0, 255) as u8;
    ui.set_console_enabled(a.console_enabled);
    ui.set_console_id(
        a.console_id
            .map(|x| format!("0x{x:X}"))
            .unwrap_or_default()
            .into(),
    );
    ui.set_console_ch(a.console_ch as i32);
    a.mode_trace = s.mode_trace;
    a.log_error_frames = s.log_error_frames;
    a.log_error_counter_changes = s.log_error_counter_changes;
    ui.set_log_error_frames(a.log_error_frames);
    ui.set_log_error_counter_changes(a.log_error_counter_changes);
    a.time_mode = s.time_mode;
    ui.set_time_mode(s.time_mode);
    a.cols_hidden = s
        .cols_hidden
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    apply_col_widths(ui, &a.cols_hidden);
    a.sim_tx_frames.clear();
    if !s.sim_widgets.trim().is_empty() {
        a.sim_widgets = serde_json::from_str(&s.sim_widgets).unwrap_or_default();
    }
    if s.trace_cap >= 1000 {
        a.trace_cap = s.trace_cap;
    }
    if s.chart_cap >= 500 {
        a.chart_cap = s.chart_cap;
    }

    let effective: Vec<String> = if !s.dbc_paths.is_empty() {
        s.dbc_paths.clone()
    } else {
        s.dbc_path.clone().into_iter().collect()
    };
    if !effective.is_empty() {
        a.dbcs.clear();
        a.dbc_paths.clear();
        a.expanded_signal_cache.clear();
        for dp in effective {
            match DbcDb::load(&dp) {
                Ok(db) => {
                    a.log(format!("加载 DBC: {}", db.file_name));
                    a.dbcs.push(db);
                    a.dbc_paths.push(dp);
                }
                Err(e) => a.log(format!("加载 DBC 失败 {dp}: {e}")),
            }
        }
        rebuild_dbc_snap(a);
    }
    a.filter = parse_filter(&s.f_id, &s.f_name, &s.f_data);
    a.filter.dir_filter = dir_idx_to_opt(s.dir_filter);
    ui.set_mode_trace(s.mode_trace);
    ui.set_f_id(s.f_id.clone().into());
    ui.set_f_name(s.f_name.clone().into());
    ui.set_f_data(s.f_data.clone().into());
    ui.set_dir_filter(s.dir_filter);
    if s.left_w > 80.0 {
        ui.set_left_w(s.left_w);
    }
    if s.bottom_h > 60.0 {
        ui.set_bottom_h(s.bottom_h);
    }
    refresh_and_reconcile_pcan(a);
}

pub(super) fn dir_idx_to_opt(idx: i32) -> Option<bool> {
    match idx {
        1 => Some(false),
        2 => Some(true),
        _ => None,
    }
}

pub(super) fn validate_filter(id_s: &str, data_s: &str) -> Result<(), String> {
    let valid_id = |s: &str| parse_u32(s).is_some_and(|n| n <= 0x1fff_ffff);
    if !id_s.trim().is_empty() {
        for token in id_s.split(',').map(str::trim) {
            let valid = if let Some(excluded) = token.strip_prefix('!') {
                valid_id(excluded)
            } else if let Some((start, end)) = token.split_once('-') {
                valid_id(start) && valid_id(end)
            } else {
                valid_id(token)
            };
            if !valid {
                return Err(format!("ID 条件错误 / Invalid ID: {token}"));
            }
        }
    }
    for byte in data_s.split_whitespace() {
        let hex = byte.strip_prefix("0x").unwrap_or(byte);
        if hex.is_empty() || hex.len() > 2 || u8::from_str_radix(hex, 16).is_err() {
            return Err(format!("Data 字节错误 / Invalid byte: {byte}"));
        }
    }
    Ok(())
}

pub(super) fn parse_filter(id_s: &str, name_s: &str, data_s: &str) -> Filter {
    let mut f = Filter::default();

    for tok in id_s.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if let Some(rest) = tok.strip_prefix('!') {
            if let Some(v) = parse_u32(rest) {
                f.deny.push(v);
            }
        } else if let Some((a, b)) = tok.split_once('-') {
            if let (Some(a), Some(b)) = (parse_u32(a.trim()), parse_u32(b.trim())) {
                f.allow.push((a.min(b), a.max(b)));
            }
        } else if let Some(v) = parse_u32(tok) {
            f.allow.push((v, v));
        }
    }

    let n = name_s.trim();
    if !n.is_empty() {
        if let Some(rest) = n.strip_prefix('!') {
            f.name = Some(rest.to_string());
            f.name_exclude = true;
        } else if let Some(rest) = n.strip_suffix('*') {
            f.name = Some(rest.to_string());
            f.name_prefix = true;
        } else if let Some(rest) = n.strip_prefix('*') {
            f.name = Some(rest.to_string());
            f.name_suffix = true;
        } else {
            f.name = Some(n.to_string());
        }
    }

    let d = data_s.trim();
    if !d.is_empty() {
        let bytes: Vec<u8> = d
            .split_whitespace()
            .filter_map(|x| u8::from_str_radix(x.trim_start_matches("0x"), 16).ok())
            .collect();
        if !bytes.is_empty() {
            f.data = Some(bytes);
        }
    }
    f
}

pub(super) fn parse_u32(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(h, 16).ok()
    } else {
        u32::from_str_radix(s, 16).ok()
    }
}

#[cfg(test)]
mod filter_validation_tests {
    use super::*;

    #[test]
    fn validates_filter_without_silently_dropping_tokens() {
        assert!(validate_filter("180,100-1FF,!200", "01 02 FF").is_ok());
        assert!(validate_filter("", "").is_ok());
        for id in ["GG", "180,", "!100-200", "20000000", "100-ZZ"] {
            assert!(validate_filter(id, "").is_err(), "{id}");
        }
        for data in ["GG", "0102", "100", "01 zz"] {
            assert!(validate_filter("180", data).is_err(), "{data}");
        }
    }
}
