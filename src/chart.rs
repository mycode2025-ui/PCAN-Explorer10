//! Chart math + the chart-window renderer.
//! Pure helpers (interpolation, windowing, range) plus `refresh_chart`, which
//! turns the live/frozen series into the `ChartSeries` model and axis labels.
//! Extracted from main.rs. Chinese text below lives only in string literals (UI data).

use crate::{App, AppWindow, ChartSeries, ChartWindow, Series, fmt_wall, sync_vec_model};
use slint::{Model, SharedString};
use std::collections::VecDeque;

/// Parse the leading numeric part of a string (e.g. "239.00A" -> 239.0).
pub(crate) fn parse_lead(s: &str) -> Option<f64> {
    let t: String = s
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
        .collect();
    t.parse().ok()
}

/// Linear interpolation over an ascending (time, value) point list.
pub(crate) fn interp_pts(pts: &[(f64, f64)], t: f64) -> f64 {
    if pts.is_empty() {
        return 0.0;
    }
    if t <= pts[0].0 {
        return pts[0].1;
    }
    let last = pts[pts.len() - 1];
    if t >= last.0 {
        return last.1;
    }
    for w in pts.windows(2) {
        let (t0, v0) = w[0];
        let (t1, v1) = w[1];
        if t >= t0 && t <= t1 {
            let f = if (t1 - t0).abs() < 1e-12 {
                0.0
            } else {
                (t - t0) / (t1 - t0)
            };
            return v0 + (v1 - v0) * f;
        }
    }
    last.1
}

fn normalized_chart_y(value: f64, low: f64, span: f64) -> f64 {
    (100.0 - (value - low) / span.max(1e-9) * 100.0).clamp(0.0, 100.0)
}

fn chart_axis_range(own_range: (f64, f64), shared_range: (f64, f64), y_mode: i32) -> (f64, f64) {
    if y_mode == 1 { shared_range } else { own_range }
}

/// Take the samples inside the visible window [tmin, tmax] (plus one neighbor on each
/// side so the polyline reaches the borders), then decimate to <= max_pts points.
/// Used to recompute the visible region at full resolution after zoom; the path and
/// the cursor share these points. Samples must be in ascending time order.
pub(crate) fn window_pts(
    samples: &VecDeque<(f64, f64)>,
    tmin: f64,
    tmax: f64,
    max_pts: usize,
) -> Vec<(f64, f64)> {
    if samples.is_empty() {
        return Vec::new();
    }
    let lower_bound = |value: f64, inclusive: bool| {
        let mut left = 0usize;
        let mut right = samples.len();
        while left < right {
            let mid = left + (right - left) / 2;
            let time = samples[mid].0;
            if time < value || (inclusive && time <= value) {
                left = mid + 1;
            } else {
                right = mid;
            }
        }
        left
    };
    let start = lower_bound(tmin, false);
    let end = lower_bound(tmax, true); // first index > tmax
    let a = start.saturating_sub(1); // include one extra neighbor on the left
    let b = (end + 1).min(samples.len()); // include one extra neighbor on the right
    if a >= b {
        return Vec::new();
    }
    decimate_min_max_deque(samples, a, b, max_pts)
}

fn decimate_min_max_deque(
    points: &VecDeque<(f64, f64)>,
    start: usize,
    end: usize,
    max_pts: usize,
) -> Vec<(f64, f64)> {
    let len = end.saturating_sub(start);
    if len == 0 || max_pts == 0 {
        return Vec::new();
    }
    if len <= max_pts {
        return (start..end).map(|index| points[index]).collect();
    }
    if max_pts == 1 {
        return vec![points[start]];
    }
    if max_pts == 2 {
        return vec![points[start], points[end - 1]];
    }

    let interior_len = len - 2;
    let bucket_count = ((max_pts - 2) / 2).max(1);
    let bucket_size = interior_len.div_ceil(bucket_count);
    let mut out = Vec::with_capacity(max_pts);
    out.push(points[start]);
    let mut bucket_start = start + 1;
    while bucket_start < end - 1 {
        let bucket_end = (bucket_start + bucket_size).min(end - 1);
        let mut min_index = bucket_start;
        let mut max_index = bucket_start;
        for index in bucket_start + 1..bucket_end {
            if points[index].1 < points[min_index].1 {
                min_index = index;
            }
            if points[index].1 > points[max_index].1 {
                max_index = index;
            }
        }
        let (first, second) = if min_index <= max_index {
            (min_index, max_index)
        } else {
            (max_index, min_index)
        };
        if out.last() != Some(&points[first]) {
            out.push(points[first]);
        }
        if second != first && out.last() != Some(&points[second]) {
            out.push(points[second]);
        }
        bucket_start = bucket_end;
    }
    let last = points[end - 1];
    if out.last() != Some(&last) {
        out.push(last);
    }
    out
}

/// Reduce a long time series without losing narrow peaks or steps. A plain
/// `step_by` can skip the only low/high sample in a bucket and make playback
/// data look incorrect; keeping each bucket's min and max preserves its envelope.
#[cfg(test)]
fn decimate_min_max(points: &[(f64, f64)], max_pts: usize) -> Vec<(f64, f64)> {
    if points.len() <= max_pts {
        return points.to_vec();
    }
    if max_pts <= 1 {
        return points.first().copied().into_iter().collect();
    }
    if max_pts == 2 {
        return vec![points[0], points[points.len() - 1]];
    }

    let interior = &points[1..points.len() - 1];
    let bucket_count = ((max_pts - 2) / 2).max(1);
    let bucket_size = interior.len().div_ceil(bucket_count);
    let mut out = Vec::with_capacity(max_pts);
    out.push(points[0]);
    for bucket in interior.chunks(bucket_size) {
        let mut min_index = 0usize;
        let mut max_index = 0usize;
        for index in 1..bucket.len() {
            if bucket[index].1 < bucket[min_index].1 {
                min_index = index;
            }
            if bucket[index].1 > bucket[max_index].1 {
                max_index = index;
            }
        }
        let (first, second) = if min_index <= max_index {
            (min_index, max_index)
        } else {
            (max_index, min_index)
        };
        if out.last() != Some(&bucket[first]) {
            out.push(bucket[first]);
        }
        if second != first && out.last() != Some(&bucket[second]) {
            out.push(bucket[second]);
        }
    }
    let last = points[points.len() - 1];
    if out.last() != Some(&last) {
        out.push(last);
    }
    out
}

/// Time range covering all visible series (falls back to 0..1 when there is no data).
pub(crate) fn chart_full_range(series: &[Series]) -> (f64, f64) {
    let mut dmin = f64::MAX;
    let mut dmax = f64::MIN;
    for s in series.iter().filter(|s| s.visible) {
        for &(t, _) in &s.samples {
            dmin = dmin.min(t);
            dmax = dmax.max(t);
        }
    }
    if dmax <= dmin {
        (0.0, 1.0)
    } else {
        (dmin, dmax)
    }
}

fn playback_original_time_label(t: f64, precise: bool) -> String {
    if t > 946_684_800.0 {
        fmt_wall(t, true)
    } else if precise {
        format!("{t:.3}s")
    } else {
        format!("{t:.1}s")
    }
}

fn chart_time_label(a: &App, t: f64, rel_base: f64) -> String {
    match (a.chart_time_source, a.chart_time_mode) {
        (0, 1) => a
            .capture_wall_epoch
            .map(|epoch| fmt_wall(epoch + t, true))
            .unwrap_or_else(|| format!("{t:.1}s")),
        (1, 1) => playback_original_time_label(t, false),
        (1, _) => format!("{:.1}s", t - rel_base),
        _ => format!("{t:.1}s"),
    }
}

fn chart_cursor_time_label(a: &App, t: f64, rel_base: f64) -> String {
    match (a.chart_time_source, a.chart_time_mode) {
        (0, 1) => a
            .capture_wall_epoch
            .map(|epoch| fmt_wall(epoch + t, true))
            .unwrap_or_else(|| format!("{t:.3}s")),
        (1, 1) => playback_original_time_label(t, true),
        (1, _) => format!("{:.3}s", t - rel_base),
        _ => format!("{t:.3}s"),
    }
}

fn chart_axis_time_label(a: &App, t: f64, rel_base: f64, visible_span: f64) -> String {
    if a.chart_time_mode == 1 {
        return chart_time_label(a, t, rel_base);
    }
    let value = if a.chart_time_source == 1 {
        t - rel_base
    } else {
        t
    };
    if visible_span >= 10.0 {
        format!("{value:.1}s")
    } else if visible_span >= 1.0 {
        format!("{value:.2}s")
    } else if visible_span >= 0.1 {
        format!("{value:.3}s")
    } else {
        format!("{value:.4}s")
    }
}

fn visible_chart_range(
    data: (f64, f64),
    chart_view: Option<(f64, f64)>,
    pause_view: Option<(f64, f64)>,
    paused: bool,
    finite_source: bool,
) -> (f64, f64) {
    let (dmin, dmax) = data;
    let mut view = if paused {
        chart_view.or(pause_view).unwrap_or(data)
    } else if finite_source {
        // Playback is a finite data set. Its selected window must stay exactly
        // where zooming/panning puts it instead of following the last sample.
        chart_view.unwrap_or(data)
    } else if let Some((vmin, vmax)) = chart_view {
        // Live capture keeps the requested width while following new samples.
        let span = (vmax - vmin).clamp(1e-9, (dmax - dmin).max(1e-9));
        ((dmax - span).max(dmin), dmax)
    } else {
        data
    };

    if dmax > dmin {
        let width = (view.1 - view.0).min(dmax - dmin).max(1e-9);
        view.0 = view.0.max(dmin).min(dmax - width);
        view.1 = view.0 + width;
    }
    view
}

/// Rebuild the chart series model + axis labels and push to both windows.
pub(crate) fn refresh_chart(a: &App, ui: &AppWindow, chart_window: &ChartWindow) {
    ui.set_chart_paused(a.chart_paused);
    ui.set_chart_normalize(a.chart_normalize);
    ui.set_chart_cursor(a.chart_cursor);
    chart_window.set_chart_paused(a.chart_paused);
    chart_window.set_chart_cursor(a.chart_cursor);
    chart_window.set_chart_time_mode(a.chart_time_mode);
    chart_window.set_chart_y_mode(a.chart_y_mode);
    chart_window.set_chart_grid(a.chart_grid);
    chart_window.set_chart_points(a.chart_points);
    // Data source: when paused use the frozen snapshot (curves stay still while new
    // samples accumulate in the background unseen); otherwise the live series.
    let series_src: &[Series] = match (a.chart_paused, a.chart_frozen_series.as_ref()) {
        (true, Some(f)) => f.as_slice(),
        _ => &a.series,
    };
    // Full time range of the data (visible series only).
    let (dmin, dmax) = chart_full_range(series_src);
    let has = series_src.iter().any(|s| s.visible) && (dmax - dmin) > 0.0;
    let (tmin, tmax) = visible_chart_range(
        (dmin, dmax),
        a.chart_view,
        a.chart_pause_view,
        a.chart_paused,
        a.chart_time_source == 1,
    );
    let tspan = (tmax - tmin).max(1e-9);
    // Highlighted signal (double-click on the project tree, valid for 2.5s).
    let hl = a
        .chart_highlight
        .as_ref()
        .filter(|(_, t)| t.elapsed().as_secs_f64() < 2.5)
        .map(|(n, _)| n.clone());
    let cursor_frac = chart_window.get_cursor_frac().clamp(0.0, 1.0) as f64;
    let cursor_time = tmin + cursor_frac * tspan;
    let cursor_frac2 = chart_window.get_cursor_frac2().clamp(0.0, 1.0) as f64;
    let cursor_time2 = tmin + cursor_frac2 * tspan;
    let dual = a.chart_dual;
    chart_window.set_chart_dual(dual);
    let y_mode = a.chart_y_mode.clamp(0, 2);
    // aspect = exact plot-area width/height for one curve. Slint Path preserves the
    // viewbox aspect ratio, so using the container width (or subtracting an estimated
    // axis width) introduces letterboxing and makes the cursor dot miss the polyline.
    let cont_w = chart_window.get_plot_w() as f64;
    let cont_h = chart_window.get_plot_h() as f64;
    let n_vis = series_src.iter().filter(|s| s.visible).count().max(1) as f64;
    const Y_AXIS_WIDTH: f64 = 66.0;
    let plot_w = (cont_w - Y_AXIS_WIDTH).max(1.0);
    let row_h = if y_mode == 0 {
        (cont_h / n_vis).max(1.0)
    } else {
        cont_h.max(1.0)
    };
    let aspect = (plot_w / row_h).clamp(0.2, 1000.0);
    chart_window.set_chart_aspect(aspect as f32);
    // Match rendering detail to the physical plot width. Two retained points per
    // horizontal pixel capture the min/max envelope without feeding a 100k-point
    // path to Slint on every refresh.
    let render_point_cap = ((plot_w.ceil() as usize) * 2).clamp(600, 4_000);

    // Build the visible point sets once. Y-axis modes reuse exactly these points so
    // the curve, cursor and reported min/max always describe the same samples.
    let point_sets: Vec<Vec<(f64, f64)>> = series_src
        .iter()
        .map(|series| window_pts(&series.samples, tmin, tmax, render_point_cap))
        .collect();
    let mut shared_min = f64::MAX;
    let mut shared_max = f64::MIN;
    for (series, points) in series_src.iter().zip(&point_sets) {
        if !series.visible {
            continue;
        }
        for &(_, value) in points {
            shared_min = shared_min.min(value);
            shared_max = shared_max.max(value);
        }
    }
    let shared_range = if shared_min == f64::MAX {
        (0.0, 1.0)
    } else if shared_max <= shared_min {
        (shared_min - 1.0, shared_min + 1.0)
    } else {
        (shared_min, shared_max)
    };
    let rows: Vec<ChartSeries> = series_src
        .iter()
        .zip(point_sets.iter())
        .map(|(s, pts)| {
            // Path and cursor share the same points so the cursor lands on the drawn curve.
            let mut smin = f64::MAX;
            let mut smax = f64::MIN;
            for &(_, v) in pts {
                smin = smin.min(v);
                smax = smax.max(v);
            }
            let own_range = if pts.is_empty() {
                (0.0, 1.0)
            } else if smax <= smin {
                (smin - 1.0, smin + 1.0)
            } else {
                (smin, smax)
            };
            // 0: each signal owns its scale; 1: all signals use one physical scale;
            // 2: each signal is normalized to 0..100% for shape comparison.
            let (axis_low, axis_high) = chart_axis_range(own_range, shared_range, y_mode);
            let margin = if y_mode == 2 {
                0.0
            } else {
                (axis_high - axis_low) * 0.08
            };
            let lo = axis_low - margin;
            let hi = axis_high + margin;
            let span = (hi - lo).max(1e-9);
            let mut cmd = String::new();
            for (k, &(t, v)) in pts.iter().enumerate() {
                let x = (t - tmin) / tspan * 100.0 * aspect;
                let y = normalized_chart_y(v, lo, span);
                if k == 0 {
                    cmd.push_str(&format!("M {x:.2} {y:.2} "));
                } else {
                    cmd.push_str(&format!("L {x:.2} {y:.2} "));
                }
            }
            // Render markers as one filled path instead of hundreds of UI elements.
            // Marker density follows the plot width so dense traces stay responsive;
            // zooming in exposes more of the original samples as individual points.
            let mut marker_cmd = String::new();
            if a.chart_points && !pts.is_empty() {
                let marker_cap = ((plot_w / 7.0).ceil() as usize).clamp(40, 320);
                let stride = pts.len().div_ceil(marker_cap).max(1);
                // About 3 px diameter at normal plot heights: visible without covering
                // narrow peaks or making dense traces look like a string of beads.
                let radius = (150.0 / row_h).clamp(0.25, 1.5);
                let circle_k = radius * 0.552_284_8;
                for (index, &(t, v)) in pts.iter().enumerate() {
                    if index % stride != 0 && index + 1 != pts.len() {
                        continue;
                    }
                    let x = (t - tmin) / tspan * 100.0 * aspect;
                    let y = normalized_chart_y(v, lo, span);
                    marker_cmd.push_str(&format!(
                        "M {:.2} {:.2} C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} Z ",
                        x + radius, y,
                        x + radius, y + circle_k, x + circle_k, y + radius, x, y + radius,
                        x - circle_k, y + radius, x - radius, y + circle_k, x - radius, y,
                        x - radius, y - circle_k, x - circle_k, y - radius, x, y - radius,
                        x + circle_k, y - radius, x + radius, y - circle_k, x + radius, y
                    ));
                }
            }
            // Cursor value: linear interpolation on the decimated polyline at cursor_time.
            let cursor_val = if a.chart_cursor && has && !pts.is_empty() {
                Some(interp_pts(&pts, cursor_time))
            } else {
                None
            };
            let (cursor_value, cursor_y, cursor_valid) = match cursor_val {
                Some(v) => (
                    format!("{:.2}{}", v, s.unit),
                    normalized_chart_y(v, lo, span) as f32,
                    true,
                ),
                None => (String::new(), 0.0, false),
            };
            // Second cursor value (dual-cursor mode).
            let cursor_val2 = if a.chart_cursor && dual && has && !pts.is_empty() {
                Some(interp_pts(&pts, cursor_time2))
            } else {
                None
            };
            let (cursor2_value, cursor2_y, cursor2_valid) = match cursor_val2 {
                Some(v) => (
                    format!("{:.2}{}", v, s.unit),
                    normalized_chart_y(v, lo, span) as f32,
                    true,
                ),
                None => (String::new(), 0.0, false),
            };
            ChartSeries {
                name: s.name.clone().into(),
                commands: cmd.into(),
                marker_commands: marker_cmd.into(),
                clr: s.color,
                cur: format!("{:.2}", s.cur).into(),
                unit: s.unit.clone().into(),
                axis_max: if y_mode == 2 {
                    "100%".into()
                } else {
                    format!("{axis_high:.2}").into()
                },
                axis_mid: if y_mode == 2 {
                    "50%".into()
                } else {
                    format!("{:.2}", (axis_low + axis_high) / 2.0).into()
                },
                axis_min: if y_mode == 2 {
                    "0%".into()
                } else {
                    format!("{axis_low:.2}").into()
                },
                cursor_value: cursor_value.into(),
                cursor_y,
                cursor_valid,
                cursor2_value: cursor2_value.into(),
                cursor2_y,
                cursor2_valid,
                visible: s.visible,
                highlight: hl.as_deref() == Some(s.name.as_str()),
                smin: if pts.is_empty() {
                    "-".into()
                } else {
                    format!("{smin:.2}").into()
                },
                smax: if pts.is_empty() {
                    "-".into()
                } else {
                    format!("{smax:.2}").into()
                },
            }
        })
        .collect();
    // Cursor readout (matches the curve points exactly: reuse each curve's interpolated value).
    let readout = if a.chart_cursor && has && dual {
        let dt = (cursor_time2 - cursor_time).abs();
        let cursor_time_text = chart_cursor_time_label(a, cursor_time, dmin);
        let cursor_time2_text = chart_cursor_time_label(a, cursor_time2, dmin);
        let mut parts = vec![if a.lang_en {
            format!("dt={dt:.3}s  (Cursor 1 {cursor_time_text} / Cursor 2 {cursor_time2_text})")
        } else {
            format!("时间差={dt:.3}s  (游标1 {cursor_time_text} / 游标2 {cursor_time2_text})")
        }];
        for r in &rows {
            if r.visible
                && r.cursor_valid
                && r.cursor2_valid
                && let (Some(v1), Some(v2)) = (
                    parse_lead(r.cursor_value.as_str()),
                    parse_lead(r.cursor2_value.as_str()),
                )
            {
                parts.push(if a.lang_en {
                    format!(
                        "{}: {} -> {}  delta={:.2}",
                        r.name,
                        r.cursor_value,
                        r.cursor2_value,
                        v2 - v1
                    )
                } else {
                    format!(
                        "{}: {} -> {}  差值={:.2}",
                        r.name,
                        r.cursor_value,
                        r.cursor2_value,
                        v2 - v1
                    )
                })
            }
        }
        parts.join("   |   ")
    } else if a.chart_cursor && has {
        let time = chart_cursor_time_label(a, cursor_time, dmin);
        let mut parts = vec![if a.lang_en {
            format!("t={time}")
        } else {
            format!("时间={time}")
        }];
        for r in &rows {
            if r.visible && r.cursor_valid {
                parts.push(format!("{}={}", r.name, r.cursor_value));
            }
        }
        parts.join("  |  ")
    } else {
        String::new()
    };
    // Update the resident chart model in place (avoid swapping the model each frame).
    {
        let m = &a.chart_model;
        while m.row_count() > rows.len() {
            m.remove(m.row_count() - 1);
        }
        for (i, row) in rows.into_iter().enumerate() {
            if i < m.row_count() {
                m.set_row_data(i, row);
            } else {
                m.push(row);
            }
        }
    }

    // Axis labels.
    let has = series_src.iter().any(|s| s.visible) && tspan > 0.0;
    ui.set_chart_ymax_label("".into());
    ui.set_chart_ymid_label("".into());
    ui.set_chart_ymin_label("".into());
    if has {
        let chart_xstart = chart_axis_time_label(a, tmin, dmin, tspan);
        let chart_xend = chart_axis_time_label(a, tmax, dmin, tspan);
        ui.set_chart_xstart_label(format!("{tmin:.1}s").into());
        ui.set_chart_xend_label(format!("{tmax:.1}s").into());
        chart_window.set_chart_xstart_label(chart_xstart.into());
        chart_window.set_chart_xend_label(chart_xend.into());
        // 5 evenly spaced time tick labels.
        let xlabels: Vec<SharedString> = (0..5)
            .map(|k| {
                chart_axis_time_label(a, tmin + (tmax - tmin) * (k as f64) / 4.0, dmin, tspan)
                    .into()
            })
            .collect();
        sync_vec_model(&a.chart_xlabel_model, xlabels);
    } else {
        ui.set_chart_xstart_label("".into());
        ui.set_chart_xend_label("".into());
        chart_window.set_chart_xstart_label("".into());
        chart_window.set_chart_xend_label("".into());
        sync_vec_model(&a.chart_xlabel_model, Vec::<SharedString>::new());
    }

    // Cursor readout.
    ui.set_chart_cursor_readout(readout.clone().into());
    chart_window.set_chart_cursor_readout(readout.into());
}

/// Advance the visible time range toward the requested zoom range. The UI timer
/// calls this at 100 ms, producing a short transition without rebuilding paths
/// multiple times inside one pointer event.
fn eased_zoom_step(current: (f64, f64), target: (f64, f64)) -> ((f64, f64), bool) {
    const EASING: f64 = 0.42;
    let next = (
        current.0 + (target.0 - current.0) * EASING,
        current.1 + (target.1 - current.1) * EASING,
    );
    let tolerance = ((target.1 - target.0).abs() * 0.002).max(1e-6);
    let finished = (next.0 - target.0).abs() <= tolerance && (next.1 - target.1).abs() <= tolerance;
    (if finished { target } else { next }, finished)
}

pub(crate) fn pan_chart_range(
    view: (f64, f64),
    data: (f64, f64),
    drag_delta_frac: f64,
) -> (f64, f64) {
    let data_span = (data.1 - data.0).max(1e-9);
    let span = (view.1 - view.0).clamp(1e-9, data_span);
    let mut start = view.0 - drag_delta_frac.clamp(-1.0, 1.0) * span;
    start = start.clamp(data.0, data.1 - span);
    (start, start + span)
}

pub(crate) fn selected_chart_range(
    view: (f64, f64),
    start_frac: f64,
    end_frac: f64,
) -> Option<(f64, f64)> {
    let start = start_frac.clamp(0.0, 1.0);
    let end = end_frac.clamp(0.0, 1.0);
    let low = start.min(end);
    let high = start.max(end);
    if high - low < 0.01 {
        return None;
    }
    let span = (view.1 - view.0).max(1e-9);
    Some((view.0 + low * span, view.0 + high * span))
}

pub(crate) fn advance_chart_zoom(a: &mut App) {
    let (Some((cur_min, cur_max)), Some((target_min, target_max))) =
        (a.chart_view, a.chart_zoom_target)
    else {
        return;
    };
    let (next, finished) = eased_zoom_step((cur_min, cur_max), (target_min, target_max));
    a.chart_view = Some(next);
    if finished {
        a.chart_zoom_target = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        chart_axis_range, decimate_min_max, eased_zoom_step, interp_pts, normalized_chart_y,
        pan_chart_range, selected_chart_range, visible_chart_range,
    };

    #[test]
    fn y_axis_modes_select_independent_or_shared_physical_ranges() {
        let own = (-20.0, 80.0);
        let shared = (-200.0, 500.0);
        assert_eq!(chart_axis_range(own, shared, 0), own);
        assert_eq!(chart_axis_range(own, shared, 1), shared);
        assert_eq!(chart_axis_range(own, shared, 2), own);
    }

    #[test]
    fn cursor_uses_the_same_interpolated_y_as_the_polyline() {
        let points = [(0.0, -10.0), (1.0, 30.0)];
        let value = interp_pts(&points, 0.25);
        let low = -20.0;
        let span = 60.0;
        let cursor_y = normalized_chart_y(value, low, span);
        let segment_y = normalized_chart_y(-10.0 + (30.0 - -10.0) * 0.25, low, span);
        assert!((cursor_y - segment_y).abs() < 1e-9);
    }

    #[test]
    fn playback_decimation_preserves_narrow_extrema_and_endpoints() {
        let mut points: Vec<(f64, f64)> = (0..2_000).map(|i| (i as f64, 10.0)).collect();
        points[987].1 = 798.0;
        points[988].1 = 0.0;
        let reduced = decimate_min_max(&points, 120);

        assert!(reduced.len() <= 120);
        assert_eq!(reduced.first(), points.first());
        assert_eq!(reduced.last(), points.last());
        assert!(reduced.iter().any(|point| point.1 == 798.0));
        assert!(reduced.iter().any(|point| point.1 == 0.0));
        assert!(reduced.windows(2).all(|pair| pair[0].0 <= pair[1].0));
    }

    #[test]
    fn zoom_transition_moves_monotonically_and_reaches_target() {
        let target = (20.0, 80.0);
        let mut current = (0.0, 100.0);
        let mut finished = false;
        for _ in 0..20 {
            let previous = current;
            (current, finished) = eased_zoom_step(current, target);
            assert!(current.0 >= previous.0 && current.1 <= previous.1);
            if finished {
                break;
            }
        }
        assert!(finished);
        assert_eq!(current, target);
    }

    #[test]
    fn pan_and_box_zoom_stay_inside_the_visible_data_range() {
        assert_eq!(
            pan_chart_range((20.0, 60.0), (0.0, 100.0), 0.25),
            (10.0, 50.0)
        );
        assert_eq!(
            pan_chart_range((20.0, 60.0), (0.0, 100.0), -2.0),
            (60.0, 100.0)
        );
        assert_eq!(
            selected_chart_range((20.0, 60.0), 0.75, 0.25),
            Some((30.0, 50.0))
        );
        assert_eq!(selected_chart_range((20.0, 60.0), 0.5, 0.505), None);
    }

    #[test]
    fn playback_keeps_the_panned_window_while_live_capture_follows_the_tail() {
        let data = (0.0, 100.0);
        let selected = Some((20.0, 50.0));
        assert_eq!(
            visible_chart_range(data, selected, None, false, true),
            (20.0, 50.0)
        );
        assert_eq!(
            visible_chart_range(data, selected, None, false, false),
            (70.0, 100.0)
        );
    }
}
