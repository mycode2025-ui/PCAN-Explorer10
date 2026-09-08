//! Message-table rendering: row builders, sort comparator, render signature, and the
//! per-frame table rebuild (`build_msg_table`). Extracted from main.rs.

use crate::dbc::Decoded;
use crate::{
    App, AppWindow, ByteCell, DISPLAY_CAP, DisplayItem, FrameRec, MsgRow, fmt_wall, id_str,
};
use slint::{Model, ModelRc, VecModel};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Default)]
pub(crate) struct TableCache {
    config: u64,
    latest: HashMap<u64, FrameRec>,
    recent: std::collections::VecDeque<FrameRec>,
    scanned_no: u64,
    trace_limit: usize,
    blocks: HashMap<u64, (u64, Vec<MsgRow>, Vec<DisplayItem>)>,
    pub(crate) refresh_ms: f64,
    pub(crate) peak_ms: f64,
    pub(crate) updated_rows: usize,
    pub(crate) ui_ms: f64,
    pub(crate) ui_peak_ms: f64,
}

impl TableCache {
    fn sync_latest(
        &mut self,
        trace: &std::collections::VecDeque<FrameRec>,
        accept: impl Fn(&FrameRec) -> bool,
    ) -> usize {
        let arrivals: Vec<_> = trace
            .iter()
            .rev()
            .take_while(|r| r.no > self.scanned_no)
            .collect();
        let inspected = arrivals.len();
        for r in arrivals.into_iter().rev() {
            if accept(r) {
                self.recent.push_back(r.clone());
                if self.recent.len() > DISPLAY_CAP {
                    self.recent.pop_front();
                }
                match self.latest.entry(r.key) {
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        if entry.get().no < r.no {
                            entry.insert(r.clone());
                        }
                    }
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(r.clone());
                    }
                }
            }
        }
        self.scanned_no = trace.back().map_or(0, |r| r.no);
        let oldest = trace.front().map_or(u64::MAX, |r| r.no);
        self.latest.retain(|_, r| r.no >= oldest);
        while self.recent.front().is_some_and(|r| r.no < oldest) {
            self.recent.pop_front();
        }
        inspected
    }
}

fn sort_decoded_for_display(signals: &mut [Decoded]) {
    signals.sort_by(|left, right| {
        left.start_bit
            .cmp(&right.start_bit)
            .then_with(|| left.name.cmp(&right.name))
    });
}

/// Build a message-table row from a frame record.
#[allow(clippy::too_many_arguments)]
pub(crate) fn make_msgrow(
    r: &FrameRec,
    hot: &[bool],
    can_expand: bool,
    expanded: bool,
    now_t: f64,
    mode_trace: bool,
    time_mode: i32,
    capture_wall_epoch: Option<f64>,
) -> MsgRow {
    let age = now_t - r.t;
    // Trace mode: briefly highlight a just-arrived frame (the new top row flashes).
    let is_new = mode_trace && (0.0..0.15).contains(&age);
    // Grouped mode: an ID not updated for ~3x its cycle is flagged as timed out.
    let timeout = !mode_trace && r.delta > 0.0 && age > (r.delta * 3.0).max(0.1);
    let frame_type = if r.error {
        "Error"
    } else if r.remote {
        "Remote"
    } else {
        "Data"
    };
    thread_local! {
        static BYTE_HEX: Vec<slint::SharedString> = (0..=255u8).map(|b| format!("{b:02X}").into()).collect();
    }
    let cells: Vec<ByteCell> = BYTE_HEX.with(|hex| {
        r.data
            .iter()
            .enumerate()
            .map(|(i, &b)| ByteCell {
                hex: hex[b as usize].clone(),
                hot: hot.get(i).copied().unwrap_or(false),
            })
            .collect()
    });
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut data_text = String::with_capacity(r.data.len() * 3);
    for (i, &byte) in r.data.iter().enumerate() {
        if i != 0 {
            data_text.push(if i % 16 == 0 { '\n' } else { ' ' });
        }
        data_text.push(HEX[(byte >> 4) as usize] as char);
        data_text.push(HEX[(byte & 15) as usize] as char);
    }
    MsgRow {
        no: r.no.to_string().into(),
        time: match time_mode {
            1 => match capture_wall_epoch {
                Some(e) => fmt_wall(e + r.t, false),
                None => format!("{:.6}", r.t),
            },
            2 => match capture_wall_epoch {
                Some(e) => fmt_wall(e + r.t, true),
                None => format!("{:.6}", r.t),
            },
            _ => format!("{:.6}", r.t),
        }
        .into(),
        delta: if r.delta > 0.0 {
            format!("{:.6}", r.delta).into()
        } else {
            "".into()
        },
        ch: format!("CAN{}", r.ch).into(),
        dir: if r.tx { "Tx" } else { "Rx" }.into(),
        id: id_str(r.id, r.ext).into(),
        name: r.name.clone().into(),
        kind: if r.ext { "Ext" } else { "Std" }.into(),
        fd: if r.fd { "Yes" } else { "No" }.into(),
        brs: if r.brs { "Yes" } else { "No" }.into(),
        dlc: format!("{}", r.data.len()).into(),
        len: format!("{}", r.data.len()).into(),
        data_bytes: ModelRc::from(Rc::new(VecModel::from(cells))),
        data_text: data_text.into(),
        cycle: if r.delta > 0.0 {
            format!("{:.1}ms", r.delta * 1000.0).into()
        } else {
            "".into()
        },
        count: format!("{}", r.count).into(),
        comment: frame_type.into(),
        is_tx: r.tx,
        is_error: r.error,
        is_new,
        timeout,
        is_signal: false,
        can_expand,
        expanded,
        signal_name: "".into(),
        signal_out_of_range: false,
    }
}

/// Build an expanded-signal sub-row (shown under its message in grouped mode).
pub(crate) fn make_signal_row(s: &Decoded, has_valid_value: bool) -> MsgRow {
    let mut value_text = if has_valid_value {
        let mut text = format!("{:.3}", s.physical);
        if !s.unit.is_empty() {
            text.push(' ');
            text.push_str(&s.unit);
        }
        text.push_str("  (Raw: ");
        text.push_str(&s.raw_text);
        text.push(')');
        if !s.enum_txt.is_empty() {
            text.push_str("  ");
            text.push_str(&s.enum_txt);
        }
        text
    } else {
        "—".to_string()
    };
    if let Some(mux) = s.mux_value {
        if s.mux_active {
            value_text.push_str(&format!("  [MUX={mux} 当前]"));
        } else if has_valid_value {
            value_text.push_str(&format!("  [MUX={mux} 上次值]"));
        } else {
            value_text.push_str(&format!("  [MUX={mux} 等待]"));
        }
    }

    MsgRow {
        no: "".into(),
        time: "".into(),
        delta: "".into(),
        ch: "".into(),
        dir: "".into(),
        id: "".into(),
        name: format!("  {}", s.name).into(),
        kind: "Sig".into(),
        fd: "".into(),
        brs: "".into(),
        dlc: "".into(),
        len: "".into(),
        data_bytes: ModelRc::from(Rc::new(VecModel::from(Vec::<ByteCell>::new()))),
        data_text: value_text.into(),
        cycle: "".into(),
        count: format!("Bit {}:{}", s.start_bit, s.size).into(),
        comment: if !s.mux_active && s.mux_value.is_some() {
            "MuxInactive"
        } else if s.out_of_range {
            "OutOfRange"
        } else {
            "Normal"
        }
        .into(),
        is_tx: false,
        is_error: false,
        is_new: false,
        timeout: false,
        is_signal: true,
        can_expand: false,
        expanded: false,
        signal_name: s.name.clone().into(),
        signal_out_of_range: s.out_of_range,
    }
}

/// Sort comparator: `col` matches the table header column index.
pub(crate) fn cmp_rec(a: &FrameRec, b: &FrameRec, col: i32) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match col {
        0 => a.no.cmp(&b.no),
        1 => a.t.partial_cmp(&b.t).unwrap_or(Ordering::Equal),
        2 => a.delta.partial_cmp(&b.delta).unwrap_or(Ordering::Equal),
        3 => a.ch.cmp(&b.ch),
        4 => a.tx.cmp(&b.tx),
        5 => a.id.cmp(&b.id),
        6 => a.name.cmp(&b.name),
        7 => a.ext.cmp(&b.ext),
        10 | 11 => a.data.len().cmp(&b.data.len()),
        14 => a.count.cmp(&b.count),
        _ => Ordering::Equal,
    }
}

/// Render signature: changes whenever any input affecting the table changes, else the
/// whole-table rebuild is skipped.
fn view_config_signature(a: &App) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    a.mode_trace.hash(&mut h);
    a.sort_col.hash(&mut h);
    a.sort_desc.hash(&mut h);
    // filter conditions
    a.filter.allow.hash(&mut h);
    a.filter.deny.hash(&mut h);
    a.filter.name.hash(&mut h);
    a.filter.name_exclude.hash(&mut h);
    a.filter.name_prefix.hash(&mut h);
    a.filter.name_suffix.hash(&mut h);
    a.filter.data.hash(&mut h);
    a.filter.dir_filter.hash(&mut h); // 方向过滤(此前漏入签名→工程加载只改方向时表不刷新)
    a.time_mode.hash(&mut h); // 时间显示模式(相对/绝对/系统)切换需重建
    a.capture_wall_epoch.map(f64::to_bits).hash(&mut h);
    // expanded set
    a.expanded_keys.len().hash(&mut h);
    let exp_sum: u64 = a
        .expanded_keys
        .iter()
        .fold(0u64, |acc, k| acc ^ k.wrapping_mul(0x9E3779B97F4A7C15));
    exp_sum.hash(&mut h);
    // whether a DBC is loaded (affects the Name column and expandability)
    a.dbcs.len().hash(&mut h);
    (std::sync::Arc::as_ptr(&a.dbc_snap) as usize).hash(&mut h);
    h.finish()
}

pub(crate) fn msg_view_signature(a: &App) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    view_config_signature(a).hash(&mut h);
    a.no_counter.hash(&mut h);
    a.trace.len().hash(&mut h);
    // 实时采集时混入 ~500ms 粗时间桶，使总线全静默期也能刷新"超时/陈旧"判断。
    if a.running
        && let Some(epoch) = a.capture_wall_epoch
        && let Ok(d) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
    {
        (((d.as_secs_f64() - epoch) * 2.0) as i64).hash(&mut h);
    }
    h.finish()
}

fn should_rebuild(paused: bool, current_signature: u64, previous_signature: u64) -> bool {
    let forced = previous_signature == u64::MAX;
    forced || (!paused && current_signature != previous_signature)
}

/// Rebuild the central message table when data changes, or when a UI action explicitly
/// requests a refresh. Pause freezes incoming-data refreshes but must not block filter/reset
/// and clear operations.
pub(crate) fn build_msg_table(a: &mut App, ui: &AppWindow) {
    let trace_limit = match ui.get_trace_window() {
        1 => 750,
        2 => DISPLAY_CAP,
        _ => 300,
    };
    if a.table_cache.trace_limit != trace_limit {
        a.table_cache.trace_limit = trace_limit;
        a.last_msg_sig = u64::MAX;
    }
    // Keep the visible trace stable while inspecting it. Capture and recording
    // continue; explicit filters/sorting/clear still refresh the frozen view.
    if a.mode_trace
        && !a.autoscroll
        && a.last_msg_sig != u64::MAX
        && a.table_cache.config == view_config_signature(a)
    {
        return;
    }
    let msg_sig = msg_view_signature(a);
    if !should_rebuild(a.paused, msg_sig, a.last_msg_sig) {
        return;
    }
    let started = std::time::Instant::now();
    let config = view_config_signature(a);
    if a.last_msg_sig == u64::MAX
        || a.table_cache.config != config
        || a.trace.back().map_or(0, |r| r.no) < a.table_cache.scanned_no
    {
        a.table_cache.latest.clear();
        a.table_cache.recent.clear();
        a.table_cache.blocks.clear();
        a.table_cache.scanned_no = 0;
        a.table_cache.config = config;
    }
    a.last_msg_sig = msg_sig;
    let filter = &a.filter;
    a.table_cache
        .sync_latest(&a.trace, |r| filter.accept(r.id, &r.name, &r.data, r.tx));
    let (rows, items, shown) = {
        let mut recs: Vec<&FrameRec> = Vec::new();
        if a.mode_trace {
            recs.extend(
                a.table_cache
                    .recent
                    .iter()
                    .skip(a.table_cache.recent.len().saturating_sub(trace_limit)),
            );
        } else {
            recs.extend(a.table_cache.latest.values());
            recs.sort_by_key(|r| r.key);
        }
        // sorting (only applied to the current display set)
        if a.sort_col >= 0 {
            let col = a.sort_col;
            let desc = a.sort_desc;
            recs.sort_by(|x, y| {
                let o = cmp_rec(x, y, col);
                if desc { o.reverse() } else { o }
            });
        }
        // Bound message delegates in grouped mode too; expanded signal rows remain
        // attached to their parent rather than being cut halfway through a message.
        recs.truncate(DISPLAY_CAP);
        let mut rows = Vec::with_capacity(recs.len());
        let mut items = Vec::with_capacity(recs.len());
        // "现在"时刻：实时采集用墙钟(总线静默也能判超时/陈旧)，否则用最新帧时间。
        let max_t = a.last.values().map(|li| li.t).fold(0.0_f64, f64::max);
        let now_t = match (a.running, a.capture_wall_epoch) {
            (true, Some(epoch)) => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| (d.as_secs_f64() - epoch).max(max_t))
                .unwrap_or(max_t),
            _ => max_t,
        };
        let mut next_blocks = HashMap::new();
        for r in &recs {
            let hot: Vec<bool> = if a.mode_trace {
                r.changed_mask.clone()
            } else {
                a.last
                    .get(&r.key)
                    .map(|li| {
                        li.byte_change_t
                            .iter()
                            .map(|&ct| {
                                let dt = r.t - ct;
                                (0.0..0.5).contains(&dt)
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };
            // Cheap message-name lookup to decide expandability (avoid full decode per row/frame).
            let can_expand = !a.mode_trace && a.dbc_message_name_frame(r.id, r.ext).is_some();
            let expanded = can_expand && a.expanded_keys.contains(&r.key);
            use std::hash::{Hash, Hasher};
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            r.no.hash(&mut hash);
            hot.hash(&mut hash);
            can_expand.hash(&mut hash);
            expanded.hash(&mut hash);
            (a.mode_trace && (0.0..0.15).contains(&(now_t - r.t))).hash(&mut hash);
            (!a.mode_trace && r.delta > 0.0 && now_t - r.t > (r.delta * 3.0).max(0.1))
                .hash(&mut hash);
            let fingerprint = hash.finish();
            let block_key = if a.mode_trace { r.no } else { r.key };
            if let Some(block) = a
                .table_cache
                .blocks
                .get(&block_key)
                .filter(|b| b.0 == fingerprint)
            {
                rows.extend(block.1.iter().cloned());
                items.extend(block.2.iter().cloned());
                next_blocks.insert(block_key, block.clone());
                continue;
            }
            let block_start = rows.len();
            rows.push(make_msgrow(
                r,
                &hot,
                can_expand,
                expanded,
                now_t,
                a.mode_trace,
                a.time_mode,
                a.capture_wall_epoch,
            ));
            items.push(DisplayItem::Message(r.key));
            if expanded {
                // Only truly-expanded rows are decoded (rare).
                let mut decoded = a.dbc_decode_all_frame(r.id, r.ext, &r.data);
                sort_decoded_for_display(&mut decoded);
                for current in decoded
                    .iter()
                    .filter(|signal| signal.mux_active && signal.mux_value.is_some())
                {
                    a.expanded_signal_cache
                        .insert((r.key, current.name.clone()), current.clone());
                }
                for current in decoded {
                    let (display, has_valid_value) = if current.mux_active {
                        (current, true)
                    } else if let Some(previous) = a
                        .expanded_signal_cache
                        .get(&(r.key, current.name.clone()))
                        .cloned()
                    {
                        let mut previous = previous;
                        previous.mux_active = false;
                        (previous, true)
                    } else {
                        (current, false)
                    };
                    items.push(DisplayItem::Signal {
                        key: r.key,
                        signal: display.name.clone(),
                    });
                    rows.push(make_signal_row(&display, has_valid_value));
                }
            }
            next_blocks.insert(
                block_key,
                (
                    fingerprint,
                    rows[block_start..].to_vec(),
                    items[block_start..].to_vec(),
                ),
            );
        }
        a.table_cache.blocks = next_blocks;
        let shown = rows.len();
        (rows, items, shown)
    };
    a.display_items = items;
    ui.set_shown_count(shown.to_string().into());
    ui.set_sort_col(a.sort_col);
    ui.set_sort_desc(a.sort_desc);
    // Update the resident model in place: align row count + per-row set_row_data,
    // preserving row delegates and click areas.
    let m = &a.msg_model;
    while m.row_count() > rows.len() {
        m.remove(m.row_count() - 1);
    }
    let mut updated = 0;
    for (i, row) in rows.into_iter().enumerate() {
        if i < m.row_count() {
            if m.row_data(i).as_ref() != Some(&row) {
                m.set_row_data(i, row);
                updated += 1;
            }
        } else {
            m.push(row);
            updated += 1;
        }
    }
    a.table_cache.updated_rows = updated;
    a.table_cache.refresh_ms = started.elapsed().as_secs_f64() * 1000.0;
    a.table_cache.peak_ms = a.table_cache.peak_ms.max(a.table_cache.refresh_ms);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(no: u64, key: u64, value: u8) -> FrameRec {
        FrameRec {
            no,
            key,
            t: no as f64 * 0.001,
            ch: 1,
            tx: false,
            id: key as u32,
            ext: false,
            fd: false,
            brs: false,
            remote: false,
            error: false,
            data: vec![value],
            delta: 0.01,
            count: no,
            changed_mask: vec![false],
            name: String::new(),
        }
    }

    #[test]
    fn grouped_refresh_scans_only_arrivals_and_expires_evicted_matches() {
        let mut cache = TableCache::default();
        let mut trace: std::collections::VecDeque<_> = (1..=100_000)
            .map(|no| frame(no, no % 200, (no % 2) as u8))
            .collect();
        assert_eq!(cache.sync_latest(&trace, |_| true), 100_000);
        assert_eq!(cache.latest.len(), 200);
        assert_eq!(cache.sync_latest(&trace, |_| true), 0);
        trace.pop_front();
        trace.push_back(frame(100_001, 1, 7));
        assert_eq!(cache.sync_latest(&trace, |_| true), 1);
        assert_eq!(cache.latest[&1].data, vec![7]);
        trace.clear();
        cache.sync_latest(&trace, |_| true);
        assert!(cache.latest.is_empty());
    }

    #[test]
    fn fd_hex_render_preserves_all_bytes_and_highlights() {
        let mut rec = frame(1, 256, 0);
        rec.data = (0..64).collect();
        let mut hot = vec![false; 64];
        hot[63] = true;
        let row = make_msgrow(&rec, &hot, false, false, rec.t, true, 0, None);
        assert_eq!(row.data_bytes.row_count(), 64);
        assert_eq!(row.data_bytes.row_data(63).unwrap().hex.as_str(), "3F");
        assert!(row.data_bytes.row_data(63).unwrap().hot);
        assert_eq!(row.data_text.lines().count(), 4);
        assert_eq!(
            row.data_text.lines().next().unwrap(),
            "00 01 02 03 04 05 06 07 08 09 0A 0B 0C 0D 0E 0F"
        );
    }

    #[test]
    fn grouped_filter_keeps_latest_matching_frame_until_evicted() {
        let mut cache = TableCache::default();
        let mut trace = std::collections::VecDeque::from(vec![frame(1, 7, 1), frame(2, 7, 0)]);
        cache.sync_latest(&trace, |r| r.data[0] == 1);
        assert_eq!(cache.latest[&7].no, 1);
        trace.push_back(frame(3, 7, 0));
        cache.sync_latest(&trace, |r| r.data[0] == 1);
        assert_eq!(cache.latest[&7].no, 1);
        trace.pop_front();
        cache.sync_latest(&trace, |r| r.data[0] == 1);
        assert!(cache.latest.is_empty());
    }

    #[test]
    fn incremental_views_match_full_scan_across_ring_rollover() {
        let mut cache = TableCache::default();
        let mut trace = std::collections::VecDeque::new();
        for batch in 0..80_u64 {
            for n in 1..=73 {
                let no = batch * 73 + n;
                trace.push_back(frame(no, no % 239, (no % 7) as u8));
                if trace.len() > 2000 {
                    trace.pop_front();
                }
            }
            let accept = |r: &FrameRec| r.data[0] != 3;
            assert_eq!(cache.sync_latest(&trace, accept), 73);
            let mut expected = HashMap::new();
            for r in trace.iter().filter(|r| accept(r)) {
                expected.insert(r.key, r.no);
            }
            assert_eq!(
                cache
                    .latest
                    .iter()
                    .map(|(&k, r)| (k, r.no))
                    .collect::<HashMap<_, _>>(),
                expected
            );
            let mut recent: Vec<_> = trace
                .iter()
                .rev()
                .filter(|r| accept(r))
                .take(DISPLAY_CAP)
                .map(|r| r.no)
                .collect();
            recent.reverse();
            assert_eq!(
                cache.recent.iter().map(|r| r.no).collect::<Vec<_>>(),
                recent
            );
        }
    }

    #[test]
    fn refresh_scan_cost_evidence() {
        let mut trace: std::collections::VecDeque<_> =
            (1..=100_000).map(|n| frame(n, n % 200, 1)).collect();
        let mut cache = TableCache::default();
        cache.sync_latest(&trace, |_| true);
        let start = std::time::Instant::now();
        for _ in 0..200 {
            let mut latest = HashMap::new();
            for r in &trace {
                latest.insert(r.key, r.no);
            }
            std::hint::black_box(latest);
        }
        let baseline = start.elapsed();
        let start = std::time::Instant::now();
        for n in 100_001..=100_200 {
            trace.pop_front();
            trace.push_back(frame(n, n % 200, 1));
            assert_eq!(cache.sync_latest(&trace, |_| true), 1);
        }
        println!(
            "200 refreshes / 100000 history: full scan {:.3} ms, incremental {:.3} ms (index only, debug build)",
            baseline.as_secs_f64() * 1000.0,
            start.elapsed().as_secs_f64() * 1000.0
        );
    }

    #[test]
    fn expanded_signal_places_value_in_data_column() {
        let decoded = Decoded {
            name: "PackCurrent".into(),
            raw: 125,
            raw_unsigned: Some(125),
            raw_text: "125".into(),
            physical: 12.5,
            unit: "A".into(),
            min: -100.0,
            max: 100.0,
            start_bit: 16,
            size: 16,
            little_endian: true,
            signed: true,
            factor: 0.1,
            offset: 0.0,
            out_of_range: false,
            enum_txt: String::new(),
            mux_active: true,
            mux_value: None,
        };

        let row = make_signal_row(&decoded, true);
        assert!(row.is_signal);
        assert_eq!(row.no.as_str(), "");
        assert_eq!(row.data_text.as_str(), "12.500 A  (Raw: 125)");
        assert_eq!(row.dlc.as_str(), "");
        assert_eq!(row.len.as_str(), "");
        assert_eq!(row.count.as_str(), "Bit 16:16");
    }

    #[test]
    fn explicit_refresh_is_not_blocked_while_paused() {
        assert!(should_rebuild(true, 42, u64::MAX));
        assert!(!should_rebuild(true, 42, 41));
        assert!(should_rebuild(false, 42, 41));
        assert!(!should_rebuild(false, 42, 42));
    }

    #[test]
    fn expanded_signals_sort_by_start_bit_then_name() {
        let make = |name: &str, start_bit: u64| Decoded {
            name: name.into(),
            raw: 0,
            raw_unsigned: Some(0),
            raw_text: "0".into(),
            physical: 0.0,
            unit: String::new(),
            min: 0.0,
            max: 0.0,
            start_bit,
            size: 1,
            little_endian: true,
            signed: false,
            factor: 1.0,
            offset: 0.0,
            out_of_range: false,
            enum_txt: String::new(),
            mux_active: true,
            mux_value: None,
        };
        let mut signals = vec![make("Z", 24), make("B", 8), make("A", 8), make("M", 0)];
        sort_decoded_for_display(&mut signals);
        assert_eq!(
            signals
                .iter()
                .map(|signal| signal.name.as_str())
                .collect::<Vec<_>>(),
            vec!["M", "A", "B", "Z"]
        );
    }
}
