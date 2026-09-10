//! python output responsibilities extracted from src/main.rs.
use super::*;

pub(super) fn reap_child(a: &mut App) {
    let timed_out = a
        .py_started
        .is_some_and(|t| t.elapsed() > std::time::Duration::from_secs(a.py_timeout_secs));
    if (a.py_stop_flag || timed_out) && a.py_child.is_some() {
        if let Some(mut c) = a.py_child.take() {
            let _ = c.kill();
        }
        a.run_status = if timed_out {
            "FAIL: 超时".into()
        } else {
            "已停止".into()
        };
        a.py_started = None;
        a.py_stop_flag = false;
        a.py_dirty = true;
        return;
    }
    a.py_stop_flag = false;
    if let Some(c) = a.py_child.as_mut()
        && let Ok(Some(st)) = c.try_wait()
    {
        let success = st.success();
        a.py_child = None;
        a.py_started = None;

        if !(a.run_status.starts_with("PASS") || a.run_status.starts_with("FAIL")) {
            a.run_status = if success {
                "PASS".into()
            } else {
                "FAIL".into()
            };
        }
        a.py_dirty = true;
    }
}

pub(super) fn run_log_path() -> std::path::PathBuf {
    std::env::temp_dir().join("pcanwork_last_run.log")
}

pub(super) fn drain_py_output(a: &mut App) {
    let mut lines = Vec::new();
    if let Some(rx) = &a.py_out_rx {
        let started = std::time::Instant::now();
        for _ in 0..1024 {
            let Ok(line) = rx.try_recv() else {
                break;
            };
            lines.push(line);
            if started.elapsed() >= std::time::Duration::from_millis(8) {
                break;
            }
        }
    }
    let dropped = a
        .py_output_dropped
        .as_ref()
        .map(|counter| counter.load(std::sync::atomic::Ordering::Relaxed))
        .unwrap_or(0);
    if dropped > a.py_output_dropped_seen {
        let newly_dropped = dropped - a.py_output_dropped_seen;
        lines.push(format!(
            "[PCAN-Explorer10] Python 输出队列已丢弃 {newly_dropped} 行（累计 {dropped}），测试结果日志不完整"
        ));
        a.py_output_dropped_seen = dropped;
        if !a.run_status.starts_with("FAIL") {
            a.run_status = "FAIL: Python 输出队列溢出".into();
        }
    }
    if lines.is_empty() {
        return;
    }
    for line in lines {
        a.py_output.push_str(&line);
        a.py_output.push('\n');
        a.log(line);
    }

    const CAP: usize = 200_000;
    if a.py_output.len() > CAP {
        let mut cut = a.py_output.len() - CAP;
        while cut < a.py_output.len() && !a.py_output.is_char_boundary(cut) {
            cut += 1;
        }
        a.py_output = a.py_output[cut..].to_string();
    }
    a.py_dirty = true;
}
