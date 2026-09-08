//! ui bridge responsibilities extracted from modbus/src/backend.rs.
use super::*;

/// Cached state of one poll window, so switching windows restores its view.
#[derive(Default, Clone)]
pub(super) struct WinCache {
    pub(super) rows: Vec<crate::RegRow>,
    pub(super) names: Vec<slint::SharedString>,
    pub(super) connected: bool,
    pub(super) status: String,
    pub(super) tx: i32,
    pub(super) rx: i32,
    pub(super) err: i32,
    pub(super) log: Vec<crate::LogLine>,
    pub(super) traffic: Vec<crate::LogLine>,
    pub(super) charts: Vec<crate::MiniChart>,
    pub(super) chart_has: bool,
    pub(super) series: Vec<crate::ChartSeries>,
    pub(super) axis_lmin: String,
    pub(super) axis_lmax: String,
    pub(super) axis_rmin: String,
    pub(super) axis_rmax: String,
    pub(super) chart_right: bool,
    pub(super) chart_page_start: i32,
    pub(super) chart_total: i32,
}

/// Per-window sink used by a master engine: updates the window's cache, pushes
/// to the flat UI properties when the window is active, and pushes to a floating
/// monitor window when one is attached.
#[derive(Clone)]
pub(super) struct WindowSink {
    pub(super) id: u32,
    pub(super) active: Arc<AtomicU32>,
    pub(super) weak: slint::Weak<crate::AppWindow>,
    pub(super) cache: Arc<Mutex<WinCache>>,
    pub(super) float: Arc<Mutex<Option<slint::Weak<crate::PollFloat>>>>,
}

impl WindowSink {
    pub(super) fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed) == self.id
    }
    pub(super) fn push(&self, f: impl FnOnce(crate::AppWindow) + Send + 'static) {
        if self.is_active() {
            let _ = self.weak.upgrade_in_event_loop(f);
        }
    }
    pub(super) fn push_float(&self, f: impl FnOnce(crate::PollFloat) + Send + 'static) {
        if let Some(fw) = self.float.lock().unwrap().clone() {
            let _ = fw.upgrade_in_event_loop(f);
        }
    }
    pub(super) fn status(&self, text: impl Into<String>, connected: bool) {
        let t = text.into();
        {
            let mut c = self.cache.lock().unwrap();
            c.status = t.clone();
            c.connected = connected;
        }
        let t2 = t.clone();
        self.push(move |a| {
            a.set_m_status(t.into());
            a.set_m_connected(connected);
        });
        self.push_float(move |f| f.set_status(t2.into()));
    }
    pub(super) fn message(&self, text: impl Into<String>) {
        let text = text.into();
        self.cache.lock().unwrap().status = text.clone();
        let float_text = text.clone();
        self.push(move |a| a.set_m_status(text.into()));
        self.push_float(move |f| f.set_status(float_text.into()));
    }
    pub(super) fn file_result(&self, action: &str, path: String, result: std::io::Result<()>) {
        let message = match result {
            Ok(()) => format!("{action}成功: {path}"),
            Err(error) => format!("{action}失败: {path} — {error}"),
        };
        self.message(message);
    }
    pub(super) fn counts(&self, tx: u64, rx: u64, err: u64) {
        let (tx, rx, err) = (tx as i32, rx as i32, err as i32);
        {
            let mut c = self.cache.lock().unwrap();
            c.tx = tx;
            c.rx = rx;
            c.err = err;
        }
        self.push(move |a| {
            a.set_m_tx(tx);
            a.set_m_rx(rx);
            a.set_m_err(err);
        });
        self.push_float(move |f| {
            f.set_tx(tx);
            f.set_rx(rx);
            f.set_err(err);
        });
    }
    pub(super) fn rows(&self, data: Vec<crate::RegRow>, names: Vec<slint::SharedString>) {
        let names_changed = {
            let mut c = self.cache.lock().unwrap();
            c.rows = data.clone();
            if c.names != names {
                c.names = names.clone();
                true
            } else {
                false
            }
        };
        let d2 = data.clone();
        self.push(move |a| a.set_m_rows(slint::ModelRc::new(slint::VecModel::from(data))));
        self.push_float(move |f| f.set_rows(slint::ModelRc::new(slint::VecModel::from(d2))));
        if names_changed {
            let n2 = names.clone();
            self.push(move |a| a.set_m_names(slint::ModelRc::new(slint::VecModel::from(names))));
            self.push_float(move |f| f.set_names(slint::ModelRc::new(slint::VecModel::from(n2))));
        }
    }
    pub(super) fn log(&self, lines: Vec<crate::LogLine>) {
        self.cache.lock().unwrap().log = lines.clone();
        let l2 = lines.clone();
        self.push(move |a| a.set_m_log(slint::ModelRc::new(slint::VecModel::from(lines))));
        self.push_float(move |f| f.set_log(slint::ModelRc::new(slint::VecModel::from(l2))));
    }
    pub(super) fn traffic(&self, lines: Vec<crate::LogLine>) {
        self.cache.lock().unwrap().traffic = lines.clone();
        let l2 = lines.clone();
        self.push(move |a| a.set_m_traffic(slint::ModelRc::new(slint::VecModel::from(lines))));
        self.push_float(move |f| f.set_traffic(slint::ModelRc::new(slint::VecModel::from(l2))));
    }
    pub(super) fn chart(&self, ch: &ChartState) {
        let (charts, has) = build_charts(ch);
        let sb = build_series(ch);
        {
            let mut c = self.cache.lock().unwrap();
            c.charts = charts.clone();
            c.chart_has = has;
            c.series = sb.series.clone();
            c.axis_lmin = sb.left_min.clone();
            c.axis_lmax = sb.left_max.clone();
            c.axis_rmin = sb.right_min.clone();
            c.axis_rmax = sb.right_max.clone();
            c.chart_right = sb.has_right;
            c.chart_page_start = ch.page_start as i32;
            c.chart_total = ch.total as i32;
        }
        let c2 = charts.clone();
        let s1 = sb.series.clone();
        let (page_start, total) = (ch.page_start as i32, ch.total as i32);
        let (lmin, lmax, rmin, rmax, hr) = (
            sb.left_min.clone(),
            sb.left_max.clone(),
            sb.right_min.clone(),
            sb.right_max.clone(),
            sb.has_right,
        );
        self.push(move |a| {
            a.set_m_charts(slint::ModelRc::new(slint::VecModel::from(charts)));
            a.set_m_chart_has(has);
            a.set_m_series(slint::ModelRc::new(slint::VecModel::from(sb.series)));
            a.set_m_axis_lmin(sb.left_min.into());
            a.set_m_axis_lmax(sb.left_max.into());
            a.set_m_axis_rmin(sb.right_min.into());
            a.set_m_axis_rmax(sb.right_max.into());
            a.set_m_chart_has_right(sb.has_right);
            a.set_m_chart_page_start(page_start);
            a.set_m_chart_total(total);
        });
        self.push_float(move |f| {
            f.set_charts(slint::ModelRc::new(slint::VecModel::from(c2)));
            f.set_chart_has(has);
            f.set_series(slint::ModelRc::new(slint::VecModel::from(s1)));
            f.set_axis_lmin(lmin.into());
            f.set_axis_lmax(lmax.into());
            f.set_axis_rmin(rmin.into());
            f.set_axis_rmax(rmax.into());
            f.set_chart_has_right(hr);
        });
    }
}

pub(super) fn push_cache_to_float(
    weak: &slint::Weak<crate::PollFloat>,
    title: &str,
    cache: &WinCache,
) {
    let c = cache.clone();
    let t = title.to_string();
    let _ = weak.upgrade_in_event_loop(move |f| {
        f.set_ptitle(t.into());
        f.set_rows(slint::ModelRc::new(slint::VecModel::from(c.rows)));
        f.set_names(slint::ModelRc::new(slint::VecModel::from(c.names)));
        f.set_status(c.status.into());
        f.set_tx(c.tx);
        f.set_rx(c.rx);
        f.set_err(c.err);
        f.set_log(slint::ModelRc::new(slint::VecModel::from(c.log)));
        f.set_traffic(slint::ModelRc::new(slint::VecModel::from(c.traffic)));
        f.set_charts(slint::ModelRc::new(slint::VecModel::from(c.charts)));
        f.set_chart_has(c.chart_has);
        f.set_series(slint::ModelRc::new(slint::VecModel::from(c.series)));
        f.set_axis_lmin(c.axis_lmin.into());
        f.set_axis_lmax(c.axis_lmax.into());
        f.set_axis_rmin(c.axis_rmin.into());
        f.set_axis_rmax(c.axis_rmax.into());
        f.set_chart_has_right(c.chart_right);
    });
}

#[derive(Clone)]
pub(super) struct UiSink {
    pub(super) weak: slint::Weak<crate::AppWindow>,
}

impl UiSink {
    pub(super) fn run(&self, f: impl FnOnce(crate::AppWindow) + Send + 'static) {
        let _ = self.weak.upgrade_in_event_loop(f);
    }

    pub(super) fn window_sink(
        &self,
        id: u32,
        active: Arc<AtomicU32>,
        cache: Arc<Mutex<WinCache>>,
        float: Arc<Mutex<Option<slint::Weak<crate::PollFloat>>>>,
    ) -> WindowSink {
        WindowSink {
            id,
            active,
            weak: self.weak.clone(),
            cache,
            float,
        }
    }

    pub(super) fn set_windows(&self, tabs: Vec<crate::WinTab>) {
        self.run(move |a| a.set_windows(slint::ModelRc::new(slint::VecModel::from(tabs))));
    }

    pub(super) fn set_active_win(&self, id: u32) {
        self.run(move |a| a.set_active_win(id as i32));
    }

    pub(super) fn push_config(&self, cfg: &UiCfg) {
        let c = cfg.clone();
        self.run(move |a| {
            a.set_m_transport(c.transport);
            a.set_m_host(c.host.into());
            a.set_m_port(c.port);
            a.set_m_serial(c.serial.into());
            a.set_m_baud_index(c.baud_index);
            a.set_m_databits_index(c.databits_index);
            a.set_m_parity(c.parity);
            a.set_m_stopbits(c.stopbits);
            a.set_m_slave_id(c.slave_id);
            a.set_m_function(c.function);
            a.set_m_address(c.address);
            a.set_m_quantity(c.quantity);
            a.set_m_scanrate(c.scanrate);
            a.set_m_format(c.format);
            a.set_scl_enabled(c.scl_enabled);
            a.set_scl_x1(c.scl_x1.into());
            a.set_scl_y1(c.scl_y1.into());
            a.set_scl_x2(c.scl_x2.into());
            a.set_scl_y2(c.scl_y2.into());
            a.set_scl_decimals(c.scl_decimals);
            a.set_col_normal(c.col_normal);
            a.set_col_op1(c.col_op1);
            a.set_col_v1(c.col_v1.into());
            a.set_col_c1(c.col_c1);
            a.set_col_op2(c.col_op2);
            a.set_col_v2(c.col_v2.into());
            a.set_col_c2(c.col_c2);
            a.set_vn_enabled(c.vn_enabled);
            a.set_vn_text(c.vn_text.into());
        });
    }

    pub(super) fn push_cache(&self, cache: &WinCache) {
        let c = cache.clone();
        self.run(move |a| {
            a.set_m_rows(slint::ModelRc::new(slint::VecModel::from(c.rows)));
            a.set_m_names(slint::ModelRc::new(slint::VecModel::from(c.names)));
            a.set_m_connected(c.connected);
            a.set_m_status(c.status.into());
            a.set_m_tx(c.tx);
            a.set_m_rx(c.rx);
            a.set_m_err(c.err);
            a.set_m_log(slint::ModelRc::new(slint::VecModel::from(c.log)));
            a.set_m_traffic(slint::ModelRc::new(slint::VecModel::from(c.traffic)));
            a.set_m_charts(slint::ModelRc::new(slint::VecModel::from(c.charts)));
            a.set_m_chart_has(c.chart_has);
            a.set_m_series(slint::ModelRc::new(slint::VecModel::from(c.series)));
            a.set_m_axis_lmin(c.axis_lmin.into());
            a.set_m_axis_lmax(c.axis_lmax.into());
            a.set_m_axis_rmin(c.axis_rmin.into());
            a.set_m_axis_rmax(c.axis_rmax.into());
            a.set_m_chart_has_right(c.chart_right);
            a.set_m_chart_page_start(c.chart_page_start);
            a.set_m_chart_total(c.chart_total);
        });
    }

    pub(super) fn master_status(&self, text: impl Into<String>, connected: bool) {
        let t = text.into();
        self.run(move |a| {
            a.set_m_status(t.into());
            a.set_m_connected(connected);
            if !connected {
                // 断开后自动写入/记录都已失效, 同步复位 UI 状态(否则界面仍显示"运行/记录中")
                a.set_wl_active(false);
                a.set_log_active(false);
            }
        });
    }

    pub(super) fn file_result(
        &self,
        server: bool,
        action: &str,
        path: String,
        result: std::io::Result<()>,
    ) {
        let message = match result {
            Ok(()) => format!("{action}成功: {path}"),
            Err(error) => format!("{action}失败: {path} — {error}"),
        };
        self.run(move |a| {
            if server {
                a.set_s_status(message.into());
            } else {
                a.set_m_status(message.into());
            }
        });
    }

    pub(super) fn scan_rows(&self, lines: Vec<crate::LogLine>) {
        self.run(move |a| a.set_scan_rows(slint::ModelRc::new(slint::VecModel::from(lines))));
    }

    pub(super) fn scan_status(&self, text: impl Into<String>, running: bool) {
        let t = text.into();
        self.run(move |a| {
            a.set_scan_status(t.into());
            a.set_scan_running(running);
        });
    }

    pub(super) fn slave_status(&self, text: impl Into<String>, running: bool) {
        let t = text.into();
        self.run(move |a| {
            a.set_s_status(t.into());
            a.set_s_running(running);
            if !running {
                a.set_sim_active(false); // 服务器停了, 模拟也停, 复位 UI
            }
        });
    }

    pub(super) fn slave_message(&self, text: impl Into<String>) {
        let text = text.into();
        self.run(move |a| a.set_s_status(text.into()));
    }

    pub(super) fn slave_requests(&self, n: u64) {
        self.run(move |a| a.set_s_requests(n as i32));
    }

    pub(super) fn slave_rows(&self, data: Vec<crate::RegRow>) {
        self.run(move |a| a.set_s_rows(slint::ModelRc::new(slint::VecModel::from(data))));
    }

    pub(super) fn slave_names(&self, names: Vec<slint::SharedString>) {
        self.run(move |a| a.set_s_names(slint::ModelRc::new(slint::VecModel::from(names))));
    }

    pub(super) fn slave_log(&self, lines: Vec<crate::LogLine>) {
        self.run(move |a| a.set_s_log(slint::ModelRc::new(slint::VecModel::from(lines))));
    }

    pub(super) fn slave_traffic(&self, lines: Vec<crate::LogLine>) {
        self.run(move |a| a.set_s_traffic(slint::ModelRc::new(slint::VecModel::from(lines))));
    }

    pub(super) fn slave_chart(&self, charts: Vec<crate::MiniChart>, has: bool) {
        self.run(move |a| {
            a.set_s_charts(slint::ModelRc::new(slint::VecModel::from(charts)));
            a.set_s_chart_has(has);
        });
    }
}
