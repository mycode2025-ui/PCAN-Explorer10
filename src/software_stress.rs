//! Debug-only synthetic load through real ingestion, DBC, table, chart and recorder paths.
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;

fn memory() -> (usize, usize) {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct Counters {
            size: u32,
            faults: u32,
            values: [usize; 9],
        }
        #[link(name = "psapi")]
        unsafe extern "system" {
            fn GetProcessMemoryInfo(process: isize, counters: *mut Counters, size: u32) -> i32;
        }
        let mut c = Counters {
            size: std::mem::size_of::<Counters>() as u32,
            faults: 0,
            values: [0; 9],
        };
        let size = c.size;
        if unsafe { GetProcessMemoryInfo(-1, &mut c, size) } != 0 {
            return (c.values[1], c.values[8]);
        }
    }
    (0, 0)
}

pub(super) fn run(
    a: &mut App,
    ui: &AppWindow,
    directory: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(directory)?;
    let mut dbc = String::from("VERSION \"\"\nNS_ :\nBS_:\nBU_: Test\n");
    for id in 256..272 {
        dbc.push_str(&format!("BO_ {id} Load{id}: 64 Test\n SG_ Mux M : 0|2@1+ (1,0) [0|3] \"\" Test\n SG_ Value : 8|16@1+ (0.1,0) [0|6553.5] \"V\" Test\n"));
        for mux in 0..4 {
            dbc.push_str(&format!(
                " SG_ Branch{mux} m{mux} : 24|16@1+ (1,0) [0|65535] \"\" Test\n"
            ));
        }
    }
    let path = directory.join("load.dbc");
    std::fs::write(&path, dbc)?;
    a.dbcs
        .push(DbcDb::load(path.to_str().ok_or("invalid path")?)?);
    rebuild_dbc_snap(a);
    for id in 256..272 {
        a.expanded_keys.insert(key_of(1, false, false, id));
        if id < 264 {
            add_signal_to_chart(a, id, "Value");
        }
    }
    let chart = ChartWindow::new()?;
    ui.set_project_open(true);
    a.running = true;
    let record_path = directory.join("load.blf");
    a.recorder.start(record_path, RecFmt::Blf)?;
    let wait = Instant::now();
    loop {
        match a.recorder.try_event() {
            Some(recording::Event::Started { .. }) => break,
            Some(recording::Event::Failed(error)) => return Err(error.into()),
            _ if wait.elapsed() > Duration::from_secs(5) => {
                return Err("recorder start timeout".into());
            }
            _ => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    a.recording = true;
    let epoch = Instant::now();
    let mut reports = Vec::new();
    for (rate, trace) in [
        (5_000_u64, false),
        (20_000, false),
        (20_000, true),
        (50_000, false),
    ] {
        a.mode_trace = trace;
        a.last_msg_sig = u64::MAX;
        let (sender, receiver) = crossbeam_channel::bounded::<Vec<CanFrame>>(1024);
        let dropped = Arc::new(AtomicU64::new(0));
        let producer_drops = dropped.clone();
        let producer = std::thread::spawn(move || {
            let start = Instant::now();
            let mut total = 0_u64;
            while start.elapsed() < Duration::from_secs(10) {
                let tick = Instant::now();
                let mut frames = Vec::new();
                for _ in 0..rate / 100 {
                    let n = total;
                    total += 1;
                    let mut data = vec![0; 64];
                    data[0] = ((n / 16) % 4) as u8;
                    data[1..3].copy_from_slice(&(n as u16).to_le_bytes());
                    data[3..5].copy_from_slice(&((n * 3) as u16).to_le_bytes());
                    frames.push(CanFrame {
                        t: epoch.elapsed().as_secs_f64(),
                        ch: 1,
                        tx: false,
                        id: 256 + (n % 16) as u32,
                        ext: false,
                        fd: true,
                        brs: true,
                        remote: false,
                        error: false,
                        data,
                    });
                }
                if let Err(error) = sender.try_send(frames) {
                    producer_drops.fetch_add(error.into_inner().len() as u64, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(10).saturating_sub(tick.elapsed()));
            }
            total
        });
        let began = Instant::now();
        let mut accepted = 0_u64;
        let mut timings = Vec::new();
        let mut table_times = Vec::new();
        let mut high = 0;
        let mut next_refresh = Instant::now();
        while !producer.is_finished() || !receiver.is_empty() {
            let tick = Instant::now();
            high = high.max(receiver.len());
            while tick.elapsed() < MAX_CAN_EVENT_TIME_PER_TICK {
                let Ok(frames) = receiver.try_recv() else {
                    break;
                };
                accepted += frames.len() as u64;
                a.ingest_batch(frames, false);
            }
            a.capture_queue_depth = receiver.len();
            a.capture_dropped_frames = dropped.load(Ordering::Relaxed);
            if Instant::now() >= next_refresh {
                refresh_ui(a, ui, None);
                refresh_chart(a, ui, &chart);
                table_times.push(a.table_cache.refresh_ms);
                next_refresh = Instant::now() + UI_REFRESH_INTERVAL;
            }
            timings.push(tick.elapsed().as_secs_f64() * 1000.0);
            if began.elapsed() > Duration::from_secs(30) {
                break;
            }
            std::thread::sleep(CAN_RECEIVE_INTERVAL.saturating_sub(tick.elapsed()));
        }
        let produced = producer.join().map_err(|_| "producer panic")?;
        let drops = dropped.load(Ordering::Relaxed);
        let undrained: u64 = receiver.try_iter().map(|frames| frames.len() as u64).sum();
        assert_eq!(
            produced,
            accepted + drops + undrained,
            "unaccounted software frames"
        );
        timings.sort_by(f64::total_cmp);
        table_times.sort_by(f64::total_cmp);
        let (working, private) = memory();
        reports.push(serde_json::json!({"trace_display_limit":if trace {match ui.get_trace_window() {1=>750,2=>1500,_=>300}} else {1500},"requested_fps":rate,"mode":if trace{"trace"}else{"group"},"seconds":began.elapsed().as_secs_f64(),"produced":produced,"accepted":accepted,"software_dropped":drops,"undrained_at_deadline":undrained,"queue_high_batches":high,"tick_p95_ms":timings[timings.len()*95/100],"tick_max_ms":timings.last(),"table_p95_ms":table_times[table_times.len()*95/100],"trace_len":a.trace.len(),"series_samples":a.series.iter().map(|s|s.samples.len()).collect::<Vec<_>>(),"working_set_bytes":working,"private_bytes":private,"recorder_drops":a.recorder.dropped_frames()}));
        std::fs::write(
            directory.join("progress.json"),
            serde_json::to_string_pretty(&reports)?,
        )?;
    }
    a.recording = false;
    a.recorder.stop()?;
    let mut recorded = None;
    let wait = Instant::now();
    while wait.elapsed() < Duration::from_secs(10) {
        match a.recorder.try_event() {
            Some(recording::Event::Stopped { frames, .. }) => {
                recorded = Some(frames);
                break;
            }
            Some(recording::Event::Failed(error)) => return Err(error.into()),
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    a.trace.clear();
    a.last.clear();
    a.series.clear();
    a.expanded_keys.clear();
    a.expanded_signal_cache.clear();
    a.last_msg_sig = u64::MAX;
    refresh_ui(a, ui, None);
    let (working, private) = memory();
    let expected_recorded: u64 = reports
        .iter()
        .map(|phase| phase["accepted"].as_u64().unwrap_or(0))
        .sum();
    std::fs::write(
        directory.join("report.json"),
        serde_json::to_string_pretty(
            &serde_json::json!({"scope":"debug software model pipeline; no hardware and no raster paint loop","phases":reports,"recorded_frames":recorded,"expected_recorded_frames":expected_recorded,"recording_complete":recorded == Some(expected_recorded),"after_clear":{"trace":a.trace.len(),"rows":a.msg_model.row_count(),"series":a.series.len(),"working_set_bytes":working,"private_bytes":private}}),
        )?,
    )?;
    if recorded != Some(expected_recorded) {
        return Err("recording incomplete: see report.json".into());
    }
    Ok(())
}
