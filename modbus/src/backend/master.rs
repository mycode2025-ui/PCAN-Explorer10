//! master responsibilities extracted from modbus/src/backend.rs.
use super::*;

pub(super) enum MasterMsg {
    Write(WriteReq),
    WriteOnce {
        func: WriteFunc,
        items: Vec<WriteItem>,
    },
    AutoWrite {
        func: WriteFunc,
        items: Vec<WriteItem>,
        interval_ms: u64,
    },
    AutoWriteStop,
    MaskWrite {
        address: u16,
        and_mask: u16,
        or_mask: u16,
    },
    ReadWrite {
        read_addr: u16,
        read_qty: u16,
        write_addr: u16,
        write_values: Vec<u16>,
    },
    SetFormat(RegFormat),
    SetName {
        address: u16,
        name: String,
    },
    SetScaling(Scaling),
    SetColors(ColorRules),
    SetValueNames(ValueNames),
    SetCellFormat {
        address: u16,
        format: Option<RegFormat>,
    },
    SetDerived(Vec<DerivedCh>),
    ExportChart(String),
    SetChartAxis {
        addr: u16,
        right: bool,
    },
    SetChartPage(i32),
    FocusChartAddress(u16),
    SetReadDef {
        area: Area,
        address: u16,
        quantity: u16,
        scan_ms: u64,
        poll: bool,
    },
    StartLog(LogCfg),
    StopLog,
    Stop,
}

pub(super) struct MasterHandle {
    pub(super) ctrl: mpsc::Sender<MasterMsg>,
    pub(super) sink: WindowSink,
}

impl MasterHandle {
    pub(super) fn send(&self, m: MasterMsg) {
        if self.ctrl.try_send(m).is_err() {
            self.sink
                .message("Master control queue full: command rejected");
        }
    }
}

pub(super) enum PollData {
    Regs(Vec<u16>),
    Bits(Vec<bool>),
}

pub(super) enum PollErr {
    Exception(String),
    Io(String),
    Invalid(String),
}

pub(super) struct ChartState {
    pub(super) addrs: Vec<u16>,
    pub(super) data: Vec<VecDeque<f64>>,
    pub(super) right: std::collections::HashSet<u16>, // addrs assigned to the right Y axis
    pub(super) page_start: usize,
    pub(super) total: usize,
}

impl ChartState {
    pub(super) fn new() -> Self {
        ChartState {
            addrs: Vec::new(),
            data: Vec::new(),
            right: std::collections::HashSet::new(),
            page_start: 0,
            total: 0,
        }
    }
    pub(super) fn reset(&mut self) {
        self.addrs.clear();
        self.data.clear();
        self.page_start = 0;
        self.total = 0;
    }
    pub(super) fn move_page(&mut self, direction: i32) -> bool {
        let max_start = self
            .total
            .saturating_sub(1)
            .div_euclid(CHART_SERIES_PER_PAGE)
            * CHART_SERIES_PER_PAGE;
        let next = if direction < 0 {
            self.page_start.saturating_sub(CHART_SERIES_PER_PAGE)
        } else if direction > 0 {
            self.page_start
                .saturating_add(CHART_SERIES_PER_PAGE)
                .min(max_start)
        } else {
            self.page_start
        };
        if next == self.page_start {
            return false;
        }
        self.page_start = next;
        true
    }
    pub(super) fn focus_address(&mut self, rows: &[DisplayRow], address: u16) -> bool {
        let Some(index) = rows
            .iter()
            .filter(|row| row.num.is_some())
            .position(|row| row.address as u16 == address)
        else {
            return false;
        };
        let next = index.div_euclid(CHART_SERIES_PER_PAGE) * CHART_SERIES_PER_PAGE;
        if next == self.page_start {
            return false;
        }
        self.page_start = next;
        true
    }
}

/// Result of building the overlay (combined) chart series + the two axis ranges.
pub(super) struct SeriesBundle {
    pub(super) series: Vec<crate::ChartSeries>,
    pub(super) left_min: String,
    pub(super) left_max: String,
    pub(super) right_min: String,
    pub(super) right_max: String,
    pub(super) has_right: bool,
}

/// Min/max over all data series assigned to the given axis (`right`).
pub(super) fn axis_range(ch: &ChartState, right: bool) -> Option<(f64, f64)> {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for (si, d) in ch.data.iter().enumerate() {
        let addr = ch.addrs.get(si).copied().unwrap_or(u16::MAX);
        if ch.right.contains(&addr) != right {
            continue;
        }
        for &v in d {
            if v < lo {
                lo = v;
            }
            if v > hi {
                hi = v;
            }
        }
    }
    if lo.is_finite() {
        Some((lo, hi))
    } else {
        None
    }
}

/// Build the overlay series, normalising each to its axis range.
pub(super) fn build_series(ch: &ChartState) -> SeriesBundle {
    let lr = axis_range(ch, false);
    let rr = axis_range(ch, true);
    let mut series = Vec::new();
    for (si, d) in ch.data.iter().enumerate() {
        if d.len() < 2 {
            continue;
        }
        let addr = ch.addrs.get(si).copied().unwrap_or(0);
        let is_right = ch.right.contains(&addr);
        let (lo, hi) = if is_right { rr } else { lr }.unwrap_or((0.0, 1.0));
        let span = if (hi - lo).abs() < 1e-9 { 1.0 } else { hi - lo };
        let n = d.len();
        let mut cmd = String::with_capacity(n * 14);
        for (i, &v) in d.iter().enumerate() {
            let norm = ((v - lo) / span).clamp(0.0, 1.0);
            let x = i as f64 / (n - 1) as f64 * 1000.0;
            let y = (1.0 - norm) * 300.0;
            if i == 0 {
                cmd.push_str(&format!("M {x:.1} {y:.1}"));
            } else {
                cmd.push_str(&format!(" L {x:.1} {y:.1}"));
            }
        }
        series.push(crate::ChartSeries {
            name: format!("@{addr}").into(),
            addr: addr as i32,
            color: argb_to_color(CHART_COLORS[si % 12]),
            axis: if is_right { 1 } else { 0 },
            commands: cmd.into(),
        });
    }
    let fmt = |o: Option<(f64, f64)>| match o {
        Some((a, b)) => (format!("{a:.2}"), format!("{b:.2}")),
        None => ("—".to_string(), "—".to_string()),
    };
    let (left_min, left_max) = fmt(lr);
    let (right_min, right_max) = fmt(rr);
    SeriesBundle {
        series,
        left_min,
        left_max,
        right_min,
        right_max,
        has_right: rr.is_some(),
    }
}

pub(super) struct Logger {
    pub(super) file: std::io::BufWriter<std::fs::File>, // 缓冲写：批量落盘，减少异步工作线程上的阻塞 syscall
    pub(super) cfg: LogCfg,
    pub(super) last_write: Option<Instant>,
    pub(super) last_flush: Option<Instant>,
    pub(super) last_vals: Option<Vec<f64>>,
    pub(super) header_written: bool,
}

impl Logger {
    pub(super) fn maybe_log(&mut self, rows: &[DisplayRow]) -> std::io::Result<()> {
        let now = Instant::now();
        let due = self.cfg.each_read
            || self
                .last_write
                .is_none_or(|t| now.duration_since(t).as_secs() >= self.cfg.period_s.max(1) as u64);
        if !due {
            return Ok(());
        }
        let nums: Vec<f64> = rows.iter().filter_map(|r| r.num).collect();
        if self.cfg.on_change && self.last_vals.as_ref() == Some(&nums) {
            return Ok(());
        }
        let d = self.cfg.delimiter.to_string();
        if !self.header_written {
            let mut h = String::new();
            if self.cfg.timestamp {
                h.push_str("Timestamp");
                h.push_str(&d);
            }
            let addrs: Vec<String> = rows
                .iter()
                .filter(|r| r.num.is_some())
                .map(|r| format!("@{}", r.address))
                .collect();
            h.push_str(&addrs.join(&d));
            writeln!(self.file, "{h}")?;
            self.header_written = true;
        }
        let mut line = String::new();
        if self.cfg.timestamp {
            line.push_str(&now_timestamp());
            line.push_str(&d);
        }
        let vals: Vec<String> = nums.iter().map(|n| format!("{n}")).collect();
        line.push_str(&vals.join(&d));
        writeln!(self.file, "{line}")?;
        // 限频 flush(~1s)：避免每轮阻塞 syscall；崩溃最多丢最近 ~1s 行，停止/Drop 时也会 flush。
        if self
            .last_flush
            .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(1))
        {
            self.file.flush()?;
            self.last_flush = Some(now);
        }
        self.last_write = Some(now);
        self.last_vals = Some(nums);
        Ok(())
    }
}

pub(super) fn spawn_master(cfg: MasterCfg, sink: WindowSink) -> MasterHandle {
    let (ctrl_tx, mut ctrl_rx) = mpsc::channel::<MasterMsg>(ENGINE_CONTROL_QUEUE_CAPACITY);
    let handle_sink = sink.clone();
    tokio::spawn(async move {
        let desc = cfg.transport.describe();
        if cfg.poll {
            if let Err(error) = validate_read_definition(cfg.area, cfg.address, cfg.quantity) {
                sink.status(format!("Invalid read definition: {error}"), false);
                return;
            }
        }
        sink.status(format!("Connecting — {desc} …"), false);

        let (mut ctx, traffic_rx, tls_desc) =
            match connect_tapped(&cfg.transport, cfg.slave_id, cfg.tls.as_ref()).await {
                Ok(c) => c,
                Err(e) => {
                    sink.status(format!("Connect failed: {e}"), false);
                    let mut lb = Vec::new();
                    sink.log(push_log(&mut lb, "ERR", e.to_string()));
                    return;
                }
            };
        let timeout_ms = if cfg.timeout_ms == 0 {
            RESPONSE_TIMEOUT_MS
        } else {
            cfg.timeout_ms
        };
        let reconnect = cfg.reconnect;
        let reconnect_ms = cfg.reconnect_ms.max(200);
        // MBAP 帧(TCP/UDP 同) vs RTU CRC: UDP 也是 MBAP，按 TCP 口径解析
        let is_tcp = matches!(cfg.transport, Transport::Tcp { .. } | Transport::Udp { .. });
        // 流量转发器: tokio JoinHandle drop 即 detach, 任务照跑; 旧连接断开时其通道关闭会自然结束
        drop(spawn_traffic_forwarder(traffic_rx, sink.clone(), is_tcp));
        let conn_desc = match &tls_desc {
            Some(td) => format!("{desc} · {td}"),
            None => desc.clone(),
        };
        if let Some(td) = &tls_desc {
            let mut lb = Vec::new();
            sink.log(push_log(&mut lb, "TX", format!("TLS established — {td}")));
        }
        sink.status(format!("Connected — {conn_desc}"), true);

        let mut tx = 0u64;
        let mut rx = 0u64;
        let mut err = 0u64;
        let mut consec_fail = 0u32; // 连续通信失败计数(用于报警/重连)
        let mut fmt = cfg.format;
        let mut scaling = cfg.scaling;
        let mut colors = cfg.colors;
        let mut value_names = cfg.value_names;
        let mut area = cfg.area;
        let mut address = cfg.address;
        let mut quantity = cfg.quantity;
        let mut scan_ms = cfg.scan_ms;
        let mut poll = cfg.poll;
        let mut last: Option<PollData> = None;
        let mut names: HashMap<u16, String> = HashMap::new();
        let mut cell_formats: HashMap<u16, RegFormat> = HashMap::new();
        let mut derived: Vec<DerivedCh> = Vec::new();
        let mut logbuf: Vec<crate::LogLine> = Vec::new();
        let mut chart = ChartState::new();
        let mut logger: Option<Logger> = None;
        // Periodic auto-write (client write list): items + whether it's running.
        let mut auto_write: Vec<WriteItem> = Vec::new();
        let mut aw_func = WriteFunc::MultiRegs;
        let mut aw_on = false;
        let mut aw_tick = tokio::time::interval(Duration::from_secs(3600));

        let mut tick = tokio::time::interval(Duration::from_millis(scan_ms.max(20)));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = aw_tick.tick(), if aw_on && !auto_write.is_empty() => {
                    tx += 1;
                    match write_items(&mut ctx, aw_func, &auto_write, timeout_ms).await {
                        Ok(n) => {
                            rx += 1;
                            sink.log(push_log(&mut logbuf, "TX", format!("Auto-write {n} registers")));
                            // advance auto-incrementing entries for the next cycle
                            for it in auto_write.iter_mut() {
                                if it.inc { it.value = it.value.wrapping_add(1); }
                            }
                        }
                        Err(e) => { err += 1; sink.log(push_log(&mut logbuf, "ERR", format!("Auto-write failed: {e}"))); }
                    }
                    sink.counts(tx, rx, err);
                }
                _ = tick.tick(), if poll => {
                    tx += 1;
                    match poll_once(&mut ctx, area, address, quantity, timeout_ms).await {
                        Ok(data) => {
                            rx += 1;
                            consec_fail = 0; // 成功一次就清零连续失败
                            let drows = render_master_rows(&data, address, fmt, &cell_formats, &scaling);
                            let (rr, nn) = master_grid(drows.clone(), &poll_values(&data), &names, &colors, &value_names, &derived, area);
                            sink.rows(rr, nn);
                            update_chart(&mut chart, &drows);
                            sink.chart(&chart);
                            let log_error = logger
                                .as_mut()
                                .and_then(|active| active.maybe_log(&drows).err());
                            if let Some(error) = log_error {
                                logger = None;
                                sink.log(push_log(
                                    &mut logbuf,
                                    "ERR",
                                    format!("Logging stopped after file write failure: {error}"),
                                ));
                            }
                            sink.log(push_log(&mut logbuf, "RX", describe_poll(area, &data)));
                            sink.status(format!("Polling — {conn_desc}"), true);
                            last = Some(data);
                        }
                        Err(PollErr::Exception(s)) => {
                            // Modbus 异常是从机的合法应答, 不是连接问题, 不计连续失败/不重连
                            err += 1;
                            sink.log(push_log(&mut logbuf, "ERR", format!("Modbus exception: {s}")));
                        }
                        Err(PollErr::Io(s)) => {
                            err += 1;
                            consec_fail += 1;
                            sink.log(push_log(&mut logbuf, "ERR", format!("Comm error: {s}")));
                            // 通信错误(超时/传输断开)：标记未连接，不再误显示"已连接"。
                            sink.status(format!("Error — {s} (连续失败 {consec_fail})"), false);
                            if consec_fail == 5 {
                                sink.log(push_log(&mut logbuf, "ERR", "⚠ 连续 5 次通信失败".into()));
                            }
                            if reconnect {
                                sink.log(push_log(&mut logbuf, "TX", format!("将在 {reconnect_ms} ms 后自动重连…")));
                                tokio::time::sleep(Duration::from_millis(reconnect_ms)).await;
                                match connect_tapped(&cfg.transport, cfg.slave_id, cfg.tls.as_ref()).await {
                                    Ok((nctx, ntraffic, _ntls)) => {
                                        ctx = nctx;
                                        drop(spawn_traffic_forwarder(ntraffic, sink.clone(), is_tcp));
                                        consec_fail = 0;
                                        sink.status(format!("Reconnected — {conn_desc}"), true);
                                        sink.log(push_log(&mut logbuf, "TX", "已重连".into()));
                                    }
                                    Err(e) => {
                                        sink.log(push_log(&mut logbuf, "ERR", format!("重连失败: {e}")));
                                    }
                                }
                            }
                        }
                        Err(PollErr::Invalid(s)) => {
                            err += 1;
                            poll = false;
                            sink.log(push_log(&mut logbuf, "ERR", format!("Invalid read definition: {s}")));
                            sink.status(format!("Invalid read definition: {s}"), true);
                        }
                    }
                    sink.counts(tx, rx, err);
                }
                msg = ctrl_rx.recv() => {
                    match msg {
                        Some(MasterMsg::Stop) | None => break,
                        Some(MasterMsg::SetFormat(f)) => {
                            fmt = f;
                            chart.reset();
                            rerender(&last, address, fmt, &scaling, &names, &colors, &value_names, &cell_formats, &derived, area, &mut chart, &sink);
                        }
                        Some(MasterMsg::SetScaling(s)) => {
                            scaling = s;
                            chart.reset();
                            rerender(&last, address, fmt, &scaling, &names, &colors, &value_names, &cell_formats, &derived, area, &mut chart, &sink);
                        }
                        Some(MasterMsg::SetColors(c)) => {
                            colors = c;
                            if let Some(d) = &last {
                                let drows = render_master_rows(d, address, fmt, &cell_formats, &scaling);
                                let (rr, nn) = master_grid(drows, &poll_values(d), &names, &colors, &value_names, &derived, area);
                                sink.rows(rr, nn);
                            }
                        }
                        Some(MasterMsg::SetValueNames(v)) => {
                            value_names = v;
                            if let Some(d) = &last {
                                let drows = render_master_rows(d, address, fmt, &cell_formats, &scaling);
                                let (rr, nn) = master_grid(drows, &poll_values(d), &names, &colors, &value_names, &derived, area);
                                sink.rows(rr, nn);
                            }
                        }
                        Some(MasterMsg::SetDerived(list)) => {
                            derived = list;
                            if let Some(d) = &last {
                                let drows = render_master_rows(d, address, fmt, &cell_formats, &scaling);
                                let (rr, nn) = master_grid(drows, &poll_values(d), &names, &colors, &value_names, &derived, area);
                                sink.rows(rr, nn);
                            }
                        }
                        Some(MasterMsg::ExportChart(path)) => {
                            let mut s = String::from("Sample");
                            for a in &chart.addrs { s.push_str(&format!(",@{a}")); }
                            s.push('\n');
                            let len = chart.data.iter().map(|d| d.len()).max().unwrap_or(0);
                            for row in 0..len {
                                s.push_str(&format!("{row}"));
                                for d in &chart.data {
                                    match d.get(row) {
                                        Some(v) => s.push_str(&format!(",{v}")),
                                        None => s.push(','),
                                    }
                                }
                                s.push('\n');
                            }
                            // 一次性导出(CSV 可能较大)移到阻塞线程池，避免卡住本引擎轮询。
                            let result_sink = sink.clone();
                            tokio::task::spawn_blocking(move || {
                                let result = std::fs::write(&path, s);
                                result_sink.file_result("图表导出", path, result);
                            });
                        }
                        Some(MasterMsg::SetChartAxis { addr: a, right }) => {
                            if right { chart.right.insert(a); } else { chart.right.remove(&a); }
                            sink.chart(&chart);
                        }
                        Some(MasterMsg::SetChartPage(direction)) => {
                            if chart.move_page(direction) {
                                if let Some(d) = &last {
                                    let drows = render_master_rows(d, address, fmt, &cell_formats, &scaling);
                                    update_chart(&mut chart, &drows);
                                }
                                sink.chart(&chart);
                            }
                        }
                        Some(MasterMsg::FocusChartAddress(addr)) => {
                            if let Some(d) = &last {
                                let drows = render_master_rows(d, address, fmt, &cell_formats, &scaling);
                                if chart.focus_address(&drows, addr) {
                                    update_chart(&mut chart, &drows);
                                    sink.chart(&chart);
                                }
                            }
                        }
                        Some(MasterMsg::SetName { address: a, name }) => {
                            if name.trim().is_empty() { names.remove(&a); } else { names.insert(a, name); }
                            if let Some(d) = &last {
                                let drows = render_master_rows(d, address, fmt, &cell_formats, &scaling);
                                let (rr, nn) = master_grid(drows, &poll_values(d), &names, &colors, &value_names, &derived, area);
                                sink.rows(rr, nn);
                            }
                        }
                        Some(MasterMsg::SetCellFormat { address: a, format }) => {
                            match format {
                                Some(f) => { cell_formats.insert(a, f); }
                                None => { cell_formats.remove(&a); }
                            }
                            chart.reset(); // value spans may change → chart series change
                            rerender(&last, address, fmt, &scaling, &names, &colors, &value_names, &cell_formats, &derived, area, &mut chart, &sink);
                        }
                        Some(MasterMsg::SetReadDef { area: na, address: nad, quantity: nq, scan_ms: ns, poll: np }) => {
                            match validate_read_definition(na, nad, nq) {
                                Ok(()) => {
                                    area = na;
                                    address = nad;
                                    quantity = nq;
                                    poll = np;
                                    if ns != scan_ms {
                                        scan_ms = ns;
                                        tick = tokio::time::interval(Duration::from_millis(scan_ms.max(20)));
                                        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                                    }
                                    chart.reset();
                                    last = None;
                                }
                                Err(error) => {
                                    sink.log(push_log(&mut logbuf, "ERR", format!("Invalid read definition: {error}")));
                                    sink.status(format!("Invalid read definition: {error}"), true);
                                }
                            }
                        }
                        Some(MasterMsg::StartLog(c)) => {
                            match std::fs::File::create(&c.path) {
                                Ok(file) => {
                                    sink.log(push_log(&mut logbuf, "TX", format!("Logging → {}", c.path)));
                                    logger = Some(Logger { file: std::io::BufWriter::new(file), cfg: c, last_write: None, last_flush: None, last_vals: None, header_written: false });
                                }
                                Err(e) => { sink.log(push_log(&mut logbuf, "ERR", format!("Log open failed: {e}"))); }
                            }
                        }
                        Some(MasterMsg::StopLog) => {
                            if logger.take().is_some() {
                                sink.log(push_log(&mut logbuf, "TX", "Logging stopped".into()));
                            }
                        }
                        Some(MasterMsg::Write(req)) => {
                            tx += 1;
                            match do_write(&mut ctx, &req, timeout_ms).await {
                                Ok(d) => { rx += 1; sink.log(push_log(&mut logbuf, "TX", d)); }
                                Err(e) => { err += 1; sink.log(push_log(&mut logbuf, "ERR", format!("Write failed: {e}"))); }
                            }
                            sink.counts(tx, rx, err);
                        }
                        Some(MasterMsg::WriteOnce { func, items }) => {
                            tx += 1;
                            match write_items(&mut ctx, func, &items, timeout_ms).await {
                                Ok(n) => { rx += 1; sink.log(push_log(&mut logbuf, "TX", format!("Wrote {n} registers"))); }
                                Err(e) => { err += 1; sink.log(push_log(&mut logbuf, "ERR", format!("Write list failed: {e}"))); }
                            }
                            sink.counts(tx, rx, err);
                        }
                        Some(MasterMsg::AutoWrite { func, items, interval_ms }) => {
                            auto_write = items;
                            aw_func = func;
                            aw_on = true;
                            aw_tick = tokio::time::interval(Duration::from_millis(interval_ms.max(50)));
                            aw_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                            sink.log(push_log(&mut logbuf, "TX", format!("Auto-write started ({} items, {interval_ms} ms)", auto_write.len())));
                        }
                        Some(MasterMsg::AutoWriteStop) => {
                            aw_on = false;
                            sink.log(push_log(&mut logbuf, "TX", "Auto-write stopped".into()));
                        }
                        Some(MasterMsg::MaskWrite { address, and_mask, or_mask }) => {
                            tx += 1;
                            let dur = Duration::from_millis(timeout_ms.max(20));
                            let r = flatten_unit(tokio::time::timeout(dur, ctx.masked_write_register(address, and_mask, or_mask)).await);
                            match r {
                                Ok(()) => { rx += 1; sink.log(push_log(&mut logbuf, "TX", format!("FC22 Mask write @{address}: AND=0x{and_mask:04X} OR=0x{or_mask:04X}"))); }
                                Err(e) => { err += 1; sink.log(push_log(&mut logbuf, "ERR", format!("FC22 Mask write failed: {e}"))); }
                            }
                            sink.counts(tx, rx, err);
                        }
                        Some(MasterMsg::ReadWrite { read_addr, read_qty, write_addr, write_values }) => {
                            let validation = validate_span(read_addr, read_qty as usize, 125, "FC23 read")
                                .and_then(|()| validate_span(write_addr, write_values.len(), 121, "FC23 write"));
                            if let Err(error) = validation {
                                err += 1;
                                sink.log(push_log(&mut logbuf, "ERR", format!("FC23 validation failed: {error}")));
                                sink.status(format!("FC23 validation failed: {error}"), true);
                                sink.counts(tx, rx, err);
                                continue;
                            }
                            tx += 1;
                            let dur = Duration::from_millis(timeout_ms.max(20));
                            match tokio::time::timeout(dur, ctx.read_write_multiple_registers(read_addr, read_qty, write_addr, &write_values)).await {
                                Ok(Ok(Ok(regs))) => {
                                    rx += 1;
                                    let shown = regs.iter().map(|r| format!("0x{r:04X}")).collect::<Vec<_>>().join(" ");
                                    sink.log(push_log(&mut logbuf, "RX", format!("FC23 wrote {} @{write_addr}, read {read_qty}@{read_addr}: {shown}", write_values.len())));
                                }
                                Ok(Ok(Err(e))) => { err += 1; sink.log(push_log(&mut logbuf, "ERR", format!("FC23 exception: {e:?}"))); }
                                Ok(Err(e)) => { err += 1; sink.log(push_log(&mut logbuf, "ERR", format!("FC23 failed: {e}"))); }
                                Err(_) => { err += 1; sink.log(push_log(&mut logbuf, "ERR", "FC23 timeout".into())); }
                            }
                            sink.counts(tx, rx, err);
                        }
                    }
                }
            }
        }

        let _ = ctx.disconnect().await;
        sink.status(format!("Disconnected — {desc}"), false);
    });

    MasterHandle {
        ctrl: ctrl_tx,
        sink: handle_sink,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn rerender(
    last: &Option<PollData>,
    start: u16,
    fmt: RegFormat,
    scaling: &Scaling,
    names: &HashMap<u16, String>,
    colors: &ColorRules,
    value_names: &ValueNames,
    overrides: &HashMap<u16, RegFormat>,
    derived: &[DerivedCh],
    area: Area,
    chart: &mut ChartState,
    sink: &WindowSink,
) {
    if let Some(d) = last {
        let drows = render_master_rows(d, start, fmt, overrides, scaling);
        let (rr, nn) = master_grid(
            drows.clone(),
            &poll_values(d),
            names,
            colors,
            value_names,
            derived,
            area,
        );
        sink.rows(rr, nn);
        update_chart(chart, &drows);
        sink.chart(chart);
    }
}

pub(super) fn render_master_rows(
    data: &PollData,
    start: u16,
    fmt: RegFormat,
    overrides: &HashMap<u16, RegFormat>,
    scaling: &Scaling,
) -> Vec<DisplayRow> {
    match data {
        PollData::Regs(v) => render_registers(start, v, fmt, overrides, scaling),
        PollData::Bits(v) => render_bits(start, v),
    }
}

pub(super) fn validate_span(
    address: u16,
    quantity: usize,
    maximum: usize,
    label: &str,
) -> Result<(), String> {
    if quantity == 0 || quantity > maximum {
        return Err(format!(
            "{label} quantity must be 1..{maximum}, got {quantity}"
        ));
    }
    if address as usize + quantity > 65_536 {
        return Err(format!(
            "{label} address range exceeds 65535: {address} + {quantity}"
        ));
    }
    Ok(())
}

pub(super) fn validate_read_definition(
    area: Area,
    address: u16,
    quantity: u16,
) -> Result<(), String> {
    let maximum = match area {
        Area::Coils | Area::DiscreteInputs => 2000,
        Area::HoldingRegisters | Area::InputRegisters => 125,
    };
    validate_span(address, quantity as usize, maximum, "read")
}

pub(super) async fn poll_once(
    ctx: &mut Context,
    area: Area,
    addr: u16,
    qty: u16,
    timeout_ms: u64,
) -> Result<PollData, PollErr> {
    validate_read_definition(area, addr, qty).map_err(PollErr::Invalid)?;
    let dur = Duration::from_millis(timeout_ms.max(20));
    match area {
        Area::Coils => match tokio::time::timeout(dur, ctx.read_coils(addr, qty)).await {
            Ok(r) => flat_bits(r),
            Err(_) => Err(PollErr::Io("Timeout".into())),
        },
        Area::DiscreteInputs => {
            match tokio::time::timeout(dur, ctx.read_discrete_inputs(addr, qty)).await {
                Ok(r) => flat_bits(r),
                Err(_) => Err(PollErr::Io("Timeout".into())),
            }
        }
        Area::HoldingRegisters => {
            match tokio::time::timeout(dur, ctx.read_holding_registers(addr, qty)).await {
                Ok(r) => flat_regs(r),
                Err(_) => Err(PollErr::Io("Timeout".into())),
            }
        }
        Area::InputRegisters => {
            match tokio::time::timeout(dur, ctx.read_input_registers(addr, qty)).await {
                Ok(r) => flat_regs(r),
                Err(_) => Err(PollErr::Io("Timeout".into())),
            }
        }
    }
}

pub(super) fn flat_regs(r: tokio_modbus::Result<Vec<u16>>) -> Result<PollData, PollErr> {
    match r {
        Ok(Ok(v)) => Ok(PollData::Regs(v)),
        Ok(Err(e)) => Err(PollErr::Exception(format!("{e:?}"))),
        Err(e) => Err(PollErr::Io(e.to_string())),
    }
}

pub(super) fn flat_bits(r: tokio_modbus::Result<Vec<bool>>) -> Result<PollData, PollErr> {
    match r {
        Ok(Ok(v)) => Ok(PollData::Bits(v)),
        Ok(Err(e)) => Err(PollErr::Exception(format!("{e:?}"))),
        Err(e) => Err(PollErr::Io(e.to_string())),
    }
}

pub(super) async fn do_write(
    ctx: &mut Context,
    req: &WriteReq,
    timeout_ms: u64,
) -> Result<String, String> {
    let dur = Duration::from_millis(timeout_ms.max(20));
    // Typed write: encode one number into registers and write via FC16.
    if let Some(fmt) = req.encode {
        // 精确整数解析 + 范围校验(不经 f64)：避免 u64 高半区被拒、>2^53 精度丢失、超范围静默饱和写错值。
        let words = encode_typed(&req.text, fmt)?;
        validate_span(req.address, words.len(), 123, "FC16 write")?;
        flatten_unit(
            tokio::time::timeout(dur, ctx.write_multiple_registers(req.address, &words)).await,
        )?;
        return Ok(format!(
            "Write {} regs @{} = {}",
            words.len(),
            req.address,
            req.text.trim()
        ));
    }
    match req.func {
        WriteFunc::SingleCoil => {
            let v = parse_bit(&req.text)
                .ok_or_else(|| "invalid coil value (use 0/1/on/off)".to_string())?;
            flatten_unit(tokio::time::timeout(dur, ctx.write_single_coil(req.address, v)).await)?;
            Ok(format!("Write Single Coil @{} = {}", req.address, v as u8))
        }
        WriteFunc::SingleReg => {
            let v = parse_word(&req.text).ok_or_else(|| "invalid register value".to_string())?;
            flatten_unit(
                tokio::time::timeout(dur, ctx.write_single_register(req.address, v)).await,
            )?;
            Ok(format!("Write Single Register @{} = {}", req.address, v))
        }
        WriteFunc::MultiCoils => {
            let vs = parse_bit_list(&req.text)
                .ok_or_else(|| "invalid coil list (e.g. 1,0,1)".to_string())?;
            validate_span(req.address, vs.len(), 1968, "FC15 write")?;
            flatten_unit(
                tokio::time::timeout(dur, ctx.write_multiple_coils(req.address, &vs)).await,
            )?;
            Ok(format!("Write {} Coils @{}", vs.len(), req.address))
        }
        WriteFunc::MultiRegs => {
            let vs = parse_word_list(&req.text)
                .ok_or_else(|| "invalid register list (e.g. 10,20,30)".to_string())?;
            validate_span(req.address, vs.len(), 123, "FC16 write")?;
            flatten_unit(
                tokio::time::timeout(dur, ctx.write_multiple_registers(req.address, &vs)).await,
            )?;
            Ok(format!("Write {} Registers @{}", vs.len(), req.address))
        }
    }
}

/// Write the list using the chosen function code. Single-* write each item in its
/// own request; Multi-* write the contiguous block in one request (base = first addr).
pub(super) fn validate_write_items(func: WriteFunc, items: &[WriteItem]) -> Result<(), String> {
    if items.is_empty() {
        return Ok(());
    }
    if matches!(func, WriteFunc::MultiRegs | WriteFunc::MultiCoils) {
        if !items
            .windows(2)
            .all(|pair| pair[0].address.checked_add(1) == Some(pair[1].address))
        {
            return Err("multiple-write addresses are not contiguous".into());
        }
        let maximum = if matches!(func, WriteFunc::MultiCoils) {
            1968
        } else {
            123
        };
        validate_span(items[0].address, items.len(), maximum, "multiple write")?;
    }
    Ok(())
}

pub(super) async fn write_items(
    ctx: &mut Context,
    func: WriteFunc,
    items: &[WriteItem],
    timeout_ms: u64,
) -> Result<usize, String> {
    let dur = Duration::from_millis(timeout_ms.max(20));
    if items.is_empty() {
        return Ok(0);
    }
    validate_write_items(func, items)?;
    let base = items[0].address;
    match func {
        WriteFunc::SingleReg => {
            for it in items {
                flatten_unit(
                    tokio::time::timeout(dur, ctx.write_single_register(it.address, it.value))
                        .await,
                )?;
            }
        }
        WriteFunc::MultiRegs => {
            let words: Vec<u16> = items.iter().map(|i| i.value).collect();
            flatten_unit(
                tokio::time::timeout(dur, ctx.write_multiple_registers(base, &words)).await,
            )?;
        }
        WriteFunc::SingleCoil => {
            for it in items {
                flatten_unit(
                    tokio::time::timeout(dur, ctx.write_single_coil(it.address, it.value != 0))
                        .await,
                )?;
            }
        }
        WriteFunc::MultiCoils => {
            let bits: Vec<bool> = items.iter().map(|i| i.value != 0).collect();
            flatten_unit(tokio::time::timeout(dur, ctx.write_multiple_coils(base, &bits)).await)?;
        }
    }
    Ok(items.len())
}

pub(super) fn flatten_unit(
    r: Result<tokio_modbus::Result<()>, tokio::time::error::Elapsed>,
) -> Result<(), String> {
    match r {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(e))) => Err(format!("{e:?}")),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("Timeout".into()),
    }
}

pub(super) fn describe_poll(area: Area, data: &PollData) -> String {
    let n = match data {
        PollData::Regs(v) => v.len(),
        PollData::Bits(v) => v.len(),
    };
    format!("Read {} ×{n}", area.label())
}

// ----- chart -----

pub(super) fn update_chart(ch: &mut ChartState, rows: &[DisplayRow]) {
    let all_points: Vec<(u16, f64)> = rows
        .iter()
        .filter_map(|r| r.num.map(|n| (r.address as u16, n)))
        .collect();
    ch.total = all_points.len();
    let max_start =
        ch.total.saturating_sub(1).div_euclid(CHART_SERIES_PER_PAGE) * CHART_SERIES_PER_PAGE;
    ch.page_start = ch.page_start.min(max_start);
    let page_end = ch
        .page_start
        .saturating_add(CHART_SERIES_PER_PAGE)
        .min(all_points.len());
    let pts = &all_points[ch.page_start..page_end];
    let addrs: Vec<u16> = pts.iter().map(|p| p.0).collect();
    if ch.addrs != addrs {
        ch.addrs = addrs;
        ch.data = ch
            .addrs
            .iter()
            .map(|_| VecDeque::with_capacity(CHART_LEN))
            .collect();
    }
    for (i, (_, n)) in pts.iter().enumerate() {
        let d = &mut ch.data[i];
        d.push_back(*n);
        while d.len() > CHART_LEN {
            d.pop_front();
        }
    }
}

/// Build one mini chart per register, each auto-scaled to its own Y range.
pub(super) fn build_charts(ch: &ChartState) -> (Vec<crate::MiniChart>, bool) {
    let mut out = Vec::with_capacity(ch.data.len());
    for (si, d) in ch.data.iter().enumerate() {
        let n = d.len();
        if n < 2 {
            continue;
        }
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for &v in d {
            if v < min {
                min = v;
            }
            if v > max {
                max = v;
            }
        }
        if !min.is_finite() {
            min = 0.0;
            max = 1.0;
        }
        if (max - min).abs() < 1e-9 {
            min -= 1.0;
            max += 1.0;
        }
        let span = max - min;
        let mut cmd = String::with_capacity(n * 14);
        for (i, &v) in d.iter().enumerate() {
            let x = i as f64 / (n - 1) as f64 * 1000.0;
            let y = 300.0 - (v - min) / span * 300.0;
            if i == 0 {
                cmd.push_str(&format!("M {x:.1} {y:.1}"));
            } else {
                cmd.push_str(&format!(" L {x:.1} {y:.1}"));
            }
        }
        out.push(crate::MiniChart {
            name: format!("@{}", ch.addrs.get(si).copied().unwrap_or(0)).into(),
            linecolor: argb_to_color(CHART_COLORS[si % 12]),
            commands: cmd.into(),
            ymin: format!("{min:.2}").into(),
            ymax: format!("{max:.2}").into(),
        });
    }
    let has = !out.is_empty();
    (out, has)
}
