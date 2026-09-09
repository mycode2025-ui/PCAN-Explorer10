// Event-wiring for the wire_chart. Included into main.rs via include!(); lives in the
// crate-root module, sharing main.rs's imports/private items (no use, no vis changes).
// Windows are passed by reference; app is an owned Rc clone. Unused params are by design.
type ChartExportSeries = Vec<(String, String, Vec<(f64, f64)>)>;

fn add_marked_curve_batch(a: &mut App) -> (usize, usize) {
    let before = a.series.len();
    let total = if a.sig_cat == 3 {
        let mut names: Vec<String> = a
            .signal_pick_expr_marked
            .iter()
            .filter(|name| a.expr_vars.iter().any(|expr| expr.name == name.as_str()))
            .cloned()
            .collect();
        names.sort();
        for name in &names {
            let _ = add_expr_to_chart(a, name);
        }
        names.len()
    } else if a.sig_cat == 0 {
        let mut signals: Vec<(u32, String)> = a
            .signal_pick_marked
            .iter()
            .filter(|(id, signal)| {
                a.dbcs.iter().any(|dbc| {
                    dbc.messages().any(|message| {
                        message.id == *id && message.signals.iter().any(|item| item.name == *signal)
                    })
                })
            })
            .cloned()
            .collect();
        signals.sort();
        for (id, signal) in &signals {
            let _ = add_signal_to_chart(a, *id, signal);
        }
        signals.len()
    } else {
        0
    };
    (a.series.len() - before, total)
}

fn log_marked_curve_batch(a: &mut App) -> bool {
    let (added, total) = add_marked_curve_batch(a);
    if total == 0 {
        a.log(if a.sig_cat == 3 {
            "请先勾选一个或多个表达式"
        } else {
            "请先勾选一个或多个 DBC 信号"
        });
        return false;
    }
    a.log(format!(
        "批量添加曲线完成: 新增 {added} 条，已存在 {} 条",
        total - added
    ));
    true
}

fn clear_all_chart_series(a: &mut App) -> usize {
    let removed = a.series.len();
    a.series.clear();
    a.chart_view = None;
    a.chart_zoom_target = None;
    a.chart_pause_view = None;
    a.chart_frozen_series = a.chart_paused.then(Vec::new);
    a.chart_highlight = None;
    removed
}

fn chart_export_snapshot(app: &App) -> ChartExportSeries {
    app.series
        .iter()
        .map(|series| {
            (
                series.name.clone(),
                series.unit.clone(),
                series.samples.iter().copied().collect(),
            )
        })
        .collect()
}

fn spawn_chart_export(
    series: ChartExportSeries,
    path: std::path::PathBuf,
    wide: bool,
    worker: WorkerSender<WorkerEvent>,
) {
    std::thread::spawn(move || {
        let result = (|| -> Result<String, String> {
            let file = std::fs::File::create(&path)
                .map_err(|error| format!("导出失败: {error}"))?;
            let mut writer = std::io::BufWriter::new(file);
            if !wide {
                writeln!(writer, "Time,Signal,Value,Unit")
                    .map_err(|error| format!("写入表头失败: {error}"))?;
                for (name, unit, samples) in &series {
                    for &(time, value) in samples {
                        writeln!(writer, "{time:.6},{name},{value},{unit}")
                            .map_err(|error| format!("写入曲线数据失败: {error}"))?;
                    }
                }
                writer
                    .flush()
                    .map_err(|error| format!("刷新曲线文件失败: {error}"))?;
                return Ok(format!("曲线数据已导出: {}", path.display()));
            }

            let mut timestamps: Vec<f64> = series
                .iter()
                .flat_map(|(_, _, samples)| samples.iter().map(|&(time, _)| time))
                .collect();
            timestamps.sort_by(|left, right| {
                left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal)
            });
            timestamps.dedup_by(|left, right| (*left - *right).abs() < 1e-9);
            const MAX_ROWS: usize = 200_000;
            let truncated = timestamps.len() > MAX_ROWS;
            timestamps.truncate(MAX_ROWS);

            let mut header = String::from("Time");
            for (name, unit, _) in &series {
                if unit.is_empty() {
                    header.push_str(&format!(",{name}"));
                } else {
                    header.push_str(&format!(",{name}({unit})"));
                }
            }
            writeln!(writer, "{header}")
                .map_err(|error| format!("写入表头失败: {error}"))?;
            let mut indices = vec![0usize; series.len()];
            let mut last = vec![None; series.len()];
            for &time in &timestamps {
                let mut line = format!("{time:.6}");
                for (index, (_, _, samples)) in series.iter().enumerate() {
                    while indices[index] < samples.len()
                        && samples[indices[index]].0 <= time + 1e-9
                    {
                        last[index] = Some(samples[indices[index]].1);
                        indices[index] += 1;
                    }
                    match last[index] {
                        Some(value) => line.push_str(&format!(",{value}")),
                        None => line.push(','),
                    }
                }
                writeln!(writer, "{line}")
                    .map_err(|error| format!("写入曲线数据失败: {error}"))?;
            }
            writer
                .flush()
                .map_err(|error| format!("刷新曲线文件失败: {error}"))?;
            let note = if truncated {
                format!("（已截断至 {MAX_ROWS} 行）")
            } else {
                String::new()
            };
            Ok(format!(
                "曲线宽表已导出({}信号 × {}行){note}: {}",
                series.len(),
                timestamps.len(),
                path.display()
            ))
        })();
        let message = result.unwrap_or_else(|error| error);
        let _ = worker.send(WorkerEvent::Log(message));
    });
}

#[allow(unused_variables, clippy::too_many_arguments)]
fn wire_chart(
    app: Rc<std::cell::RefCell<App>>,
    ui: &AppWindow,
    chart_window: &ChartWindow,
    signal_window: &SignalSelectWindow,
    tx_window: &TxWindow,
    channel_window: &ChannelConfigWindow,
    playback_window: &PlaybackWindow,
    convert_window: &ConvertWindow,
    cache_window: &CacheConfigWindow,
    trigger_window: &TriggerWindow,
    sim_panel_window: &SimPanelWindow,
    sim_prop_window: &SimPropWindow,
) {
    {
        let app = app.clone();
        chart_window.on_chart_toggle_pause(move || {
            let mut a = app.borrow_mut();
            a.chart_paused = !a.chart_paused;
            if a.chart_paused {
                a.chart_pause_view = Some(a.chart_view.unwrap_or_else(|| chart_full_range(&a.series)));
                a.chart_frozen_series = Some(a.series.clone());
            } else {
                a.chart_pause_view = None;
                a.chart_frozen_series = None;
            }
        });
    }
    {
        let cw = chart_window.as_weak();
        chart_window.on_chart_topmost_toggle(move |on| {
            if let Some(w) = cw.upgrade() {
                set_window_topmost(w.window(), on);
            }
        });
    }
    {
        let app = app.clone();
        let uiw = ui.as_weak();
        let chartw = chart_window.as_weak();
        chart_window.on_chart_set_time_mode(move |mode| {
            let mut a = app.borrow_mut();
            a.chart_time_mode = mode.clamp(0, 1);
            let (Some(ui), Some(cw)) = (uiw.upgrade(), chartw.upgrade()) else {
                return;
            };
            refresh_chart(&a, &ui, &cw);
        });
    }
    {
        let app = app.clone();
        let uiw = ui.as_weak();
        let chartw = chart_window.as_weak();
        chart_window.on_chart_set_y_mode(move |mode| {
            let mut a = app.borrow_mut();
            a.chart_y_mode = mode.clamp(0, 2);
            let (Some(ui), Some(cw)) = (uiw.upgrade(), chartw.upgrade()) else {
                return;
            };
            refresh_chart(&a, &ui, &cw);
        });
    }
    {
        let app = app.clone();
        chart_window.on_chart_grid_toggle(move |visible| {
            app.borrow_mut().chart_grid = visible;
        });
    }
    {
        let app = app.clone();
        let uiw = ui.as_weak();
        let chartw = chart_window.as_weak();
        chart_window.on_chart_points_toggle(move |visible| {
            let mut a = app.borrow_mut();
            a.chart_points = visible;
            let (Some(ui), Some(cw)) = (uiw.upgrade(), chartw.upgrade()) else {
                return;
            };
            refresh_chart(&a, &ui, &cw);
        });
    }
    {
        let app = app.clone();
        chart_window.on_clear_chart(move || {
            let mut a = app.borrow_mut();
            for series in &mut a.series {
                series.samples.clear();
                series.cur = 0.0;
            }
            a.chart_view = None;
            a.chart_zoom_target = None;
            a.chart_pause_view = None;
            let frozen = a.chart_paused.then(|| a.series.clone());
            a.chart_frozen_series = frozen;
            a.log("已清空曲线数据，保留已选信号");
        });
    }
    // 时间轴滚轮缩
{
        let app = app.clone();
        chart_window.on_chart_zoom(move |delta, frac| {
            let mut a = app.borrow_mut();
            // 当前窗口（未缩放则取「当前显示的数据集」全程：暂停时用冻结快照，否则用实时数据
            let current_view = a.chart_view.unwrap_or_else(|| {
                let src: &[Series] = if a.chart_paused {
                    a.chart_frozen_series.as_deref().unwrap_or(&a.series)
                } else {
                    &a.series
                };
                chart_full_range(src)
            });
            let (mut vmin, mut vmax) = a.chart_zoom_target.unwrap_or(current_view);
            let span = (vmax - vmin).max(1e-6);
            let center = vmin + (frac as f64).clamp(0.0, 1.0) * span;
            // delta<0（向上滚）放大，>0 缩小
            let factor = if delta < 0.0 { 0.86 } else { 1.16 };
            // 数据范围：窗口不得超出数据，否则波形挤在中间、游标按整宽走就对不
let src: &[Series] = if a.chart_paused {
                a.chart_frozen_series.as_deref().unwrap_or(&a.series)
            } else {
                &a.series
            };
            let (dmin, dmax) = chart_full_range(src);
            let data_span = (dmax - dmin).max(1e-6);
            let mut new_span = (span * factor).clamp(0.02, data_span); // 不能比数据全程还
new_span = new_span.min(data_span);
            vmin = center - (frac as f64) * new_span;
            vmax = vmin + new_span;
            // 平移回数据范围内，保证波形始终铺满绘图区宽度
            if vmin < dmin {
                vmin = dmin;
                vmax = dmin + new_span;
            }
            if vmax > dmax {
                vmax = dmax;
                vmin = dmax - new_span;
            }
            if a.chart_view.is_none() {
                a.chart_view = Some(current_view);
            }
            a.chart_zoom_target = Some((vmin, vmax));
        });
    }
    {
        let app = app.clone();
        chart_window.on_chart_pan(move |delta_frac| {
            let mut a = app.borrow_mut();
            let src: &[Series] = if a.chart_paused {
                a.chart_frozen_series.as_deref().unwrap_or(&a.series)
            } else {
                &a.series
            };
            let data_range = chart_full_range(src);
            let current = a.chart_zoom_target.or(a.chart_view).unwrap_or(data_range);
            a.chart_view = Some(pan_chart_range(current, data_range, delta_frac as f64));
            a.chart_zoom_target = None;
        });
    }
    {
        let app = app.clone();
        chart_window.on_chart_zoom_selection(move |start_frac, end_frac| {
            let mut a = app.borrow_mut();
            let src: &[Series] = if a.chart_paused {
                a.chart_frozen_series.as_deref().unwrap_or(&a.series)
            } else {
                &a.series
            };
            let data_range = chart_full_range(src);
            let current = a.chart_zoom_target.or(a.chart_view).unwrap_or(data_range);
            if let Some(target) =
                selected_chart_range(current, start_frac as f64, end_frac as f64)
            {
                if a.chart_view.is_none() {
                    a.chart_view = Some(current);
                }
                a.chart_zoom_target = Some(target);
            }
        });
    }
    // 适应（重置缩放为全程
{
        let app = app.clone();
        chart_window.on_chart_fit(move || {
            let mut a = app.borrow_mut();
            a.chart_view = None;
            a.chart_zoom_target = None;
        });
    }
    {
        let app = app.clone();
        chart_window.on_chart_cursor_toggle(move || {
            let mut a = app.borrow_mut();
            a.chart_cursor = !a.chart_cursor;
        });
    }
    {
        let app = app.clone();
        chart_window.on_chart_dual_toggle(move || {
            let mut a = app.borrow_mut();
            a.chart_dual = !a.chart_dual;
            if a.chart_dual {
                a.chart_cursor = true; // 双游标需先有游标
            }
        });
    }
    // 游标随鼠标移动即时重算（不等 100ms 定时器，消除拖动滞后
{
        let app = app.clone();
        let uiw = ui.as_weak();
        let chartw = chart_window.as_weak();
        chart_window.on_chart_cursor_move(move || {
            let (Some(ui), Some(cw)) = (uiw.upgrade(), chartw.upgrade()) else {
                return;
            };
            let a = app.borrow();
            refresh_chart(&a, &ui, &cw);
        });
    }
    {
        let app = app.clone();
        chart_window.on_chart_toggle_series(move |i| {
            let mut a = app.borrow_mut();
            if let Some(s) = a.series.get_mut(i as usize) {
                s.visible = !s.visible;
            }
        });
    }
    {
        let app = app.clone();
        chart_window.on_chart_remove_series(move |i| {
            let mut a = app.borrow_mut();
            let i = i as usize;
            if i < a.series.len() {
                let name = a.series.remove(i).name;
                a.log(format!("已移除曲线信号 {name}"));
            }
        });
    }
    {
        let app = app.clone();
        chart_window.on_clear_chart_series(move || {
            let mut a = app.borrow_mut();
            let removed = clear_all_chart_series(&mut a);
            a.log(format!("已清空 {removed} 条曲线"));
        });
    }
    {
        let app = app.clone();
        signal_window.on_signal_pick_search(move |s| {
            let mut a = app.borrow_mut();
            a.signal_pick_filter = s.to_string();
            if !a.signal_pick_filter.trim().is_empty() {
                a.signal_pick_root_open = true;
                a.signal_pick_messages_open = true;
            }
        });
    }
    {
        let app = app.clone();
        let picker = signal_window.as_weak();
        signal_window.on_signal_pick_row_clicked(move |i| {
            let mut a = app.borrow_mut();
            let Some(item) = a.signal_pick_items.get(i as usize).cloned() else {
                return;
            };
            match item {
                SignalPickItem::DbcRoot => a.signal_pick_root_open = !a.signal_pick_root_open,
                SignalPickItem::MessagesRoot => {
                    a.signal_pick_messages_open = !a.signal_pick_messages_open
                }
                SignalPickItem::Message(id) => {
                    if !a.signal_pick_msg_expanded.insert(id) {
                        a.signal_pick_msg_expanded.remove(&id);
                    }
                }
                SignalPickItem::Signal(id, signal) => {
                    a.signal_pick_selected = Some((id, signal.clone()));
                    if !a.signal_pick_marked.insert((id, signal.clone())) {
                        a.signal_pick_marked.remove(&(id, signal));
                    }
                }
                SignalPickItem::ExprVar(name) => {
                    a.signal_pick_expr_selected = Some(name.clone());
                    if !a.signal_pick_expr_marked.insert(name.clone()) {
                        a.signal_pick_expr_marked.remove(&name);
                    }
                    // 选中即把该表达式填进编辑栏, 方便修改
                    if let Some(w) = picker.upgrade()
                        && let Some(ev) = a.expr_vars.iter().find(|e| e.name == name)
                    {
                        w.set_expr_name(ev.name.clone().into());
                        w.set_expr_formula(ev.formula.clone().into());
                        w.set_expr_unit(ev.unit.clone().into());
                        w.set_expr_error("".into());
                    }
                }
            }
        });
    }
    {
        let app = app.clone();
        signal_window.on_signal_pick_row_double_clicked(move |i| {
            let mut a = app.borrow_mut();
            let Some(item) = a.signal_pick_items.get(i as usize).cloned() else {
                return;
            };
            match item {
                SignalPickItem::Signal(id, signal) => {
                    a.signal_pick_selected = Some((id, signal.clone()));
                    a.signal_pick_marked.insert((id, signal.clone()));
                    let msg = add_signal_to_chart(&mut a, id, &signal);
                    a.log(msg);
                }
                SignalPickItem::ExprVar(name) => {
                    a.signal_pick_expr_selected = Some(name.clone());
                    a.signal_pick_expr_marked.insert(name.clone());
                    let msg = add_expr_to_chart(&mut a, &name);
                    a.log(msg);
                }
                SignalPickItem::Message(id) => {
                    if !a.signal_pick_msg_expanded.insert(id) {
                        a.signal_pick_msg_expanded.remove(&id);
                    }
                }
                SignalPickItem::DbcRoot => a.signal_pick_root_open = !a.signal_pick_root_open,
                SignalPickItem::MessagesRoot => {
                    a.signal_pick_messages_open = !a.signal_pick_messages_open
                }
            }
        });
    }
    {
        let app = app.clone();
        signal_window.on_signal_pick_select_all(move || {
            let mut a = app.borrow_mut();
            let items = a.signal_pick_items.clone();
            if a.sig_cat == 3 {
                for item in items {
                    if let SignalPickItem::ExprVar(name) = item {
                        a.signal_pick_expr_marked.insert(name);
                    }
                }
            } else if a.sig_cat == 0 {
                for item in items {
                    if let SignalPickItem::Signal(id, signal) = item {
                        a.signal_pick_marked.insert((id, signal));
                    }
                }
            }
        });
    }
    {
        let app = app.clone();
        signal_window.on_signal_pick_clear_selection(move || {
            let mut a = app.borrow_mut();
            if a.sig_cat == 3 {
                a.signal_pick_expr_marked.clear();
            } else if a.sig_cat == 0 {
                a.signal_pick_marked.clear();
            }
        });
    }
    {
        let app = app.clone();
        let picker = signal_window.as_weak();
        signal_window.on_signal_pick_ok(move || {
            let mut a = app.borrow_mut();
            let added = log_marked_curve_batch(&mut a);
            if added && let Some(picker) = picker.upgrade() {
                let _ = picker.hide();
            }
        });
    }
    {
        let app = app.clone();
        signal_window.on_signal_pick_apply(move || {
            let mut a = app.borrow_mut();
            let _ = log_marked_curve_batch(&mut a);
        });
    }
    {
        let app = app.clone();
        signal_window.on_sig_cat_changed(move |cat| {
            app.borrow_mut().sig_cat = cat;
        });
    }
    {
        let app = app.clone();
        let picker = signal_window.as_weak();
        signal_window.on_expr_save(move || {
            let Some(w) = picker.upgrade() else { return; };
            let name = w.get_expr_name().trim().to_string();
            let formula = w.get_expr_formula().trim().to_string();
            let unit = w.get_expr_unit().trim().to_string();
            if name.is_empty() || formula.is_empty() {
                w.set_expr_error("名称和表达式不能为空".into());
                return;
            }
            let refs = match crate::expr::refs(&formula) {
                Ok(r) => r,
                Err(e) => {
                    w.set_expr_error(format!("表达式错误: {e}").into());
                    return;
                }
            };
            let mut a = app.borrow_mut();
            let unknown: Vec<String> = refs.into_iter().filter(|r| !a.dbc_has_signal(r)).collect();
            // 添加或按名更新
            if let Some(ev) = a.expr_vars.iter_mut().find(|e| e.name == name) {
                ev.formula = formula.clone();
                ev.unit = unit.clone();
            } else {
                a.expr_vars.push(ExprVar { name: name.clone(), formula: formula.clone(), unit: unit.clone() });
            }
            // 已在曲线里的同名表达式系列同步公式/单位
            for s in a.series.iter_mut().filter(|s| s.expr.is_some() && s.name == name) {
                s.expr = Some(formula.clone());
                s.unit = unit.clone();
            }
            recompute_expr_ids(&mut a);
            a.signal_pick_expr_selected = Some(name.clone());
            a.signal_pick_expr_marked.insert(name.clone());
            let warn = if unknown.is_empty() { String::new() } else { format!("（DBC 中暂无: {}）", unknown.join(", ")) };
            a.log(format!("表达式已保存: {name} = {formula}{warn}"));
            drop(a);
            w.set_expr_error(if unknown.is_empty() { "".into() } else { format!("已保存。DBC 中暂无信号: {}（取 0）", unknown.join(", ")).into() });
        });
    }
    {
        let app = app.clone();
        let picker = signal_window.as_weak();
        signal_window.on_expr_delete(move || {
            let mut a = app.borrow_mut();
            let Some(name) = a.signal_pick_expr_selected.clone() else {
                a.log("请先在列表里选择要删除的表达式");
                return;
            };
            a.expr_vars.retain(|e| e.name != name);
            a.signal_pick_expr_selected = None;
            a.signal_pick_expr_marked.remove(&name);
            recompute_expr_ids(&mut a);
            a.log(format!("已删除表达式: {name}"));
            drop(a);
            if let Some(w) = picker.upgrade() {
                w.set_expr_name("".into());
                w.set_expr_formula("".into());
                w.set_expr_unit("".into());
                w.set_expr_error("".into());
            }
        });
    }
    {
        let picker = signal_window.as_weak();
        signal_window.on_signal_pick_cancel(move || {
            if let Some(picker) = picker.upgrade() {
                let _ = picker.hide();
            }
        });
    }
}
