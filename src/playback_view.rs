//! playback view responsibilities extracted from src/main.rs.
use super::*;

pub(super) fn playback_online_mode(requested: bool, connected: bool) -> bool {
    requested && connected
}

pub(super) fn pb_apply_files(a: &mut App, w: &PlaybackWindow) {
    let concat = w.get_merge_concat();
    let mut out: Vec<CanFrame> = Vec::new();
    if concat {
        let mut cursor = 0.0_f64;
        for (_, fr) in &a.pb_files {
            if fr.is_empty() {
                continue;
            }
            let fmin = fr.iter().map(|f| f.t).fold(f64::INFINITY, f64::min);
            let fmax = fr.iter().map(|f| f.t).fold(f64::NEG_INFINITY, f64::max);
            let shift = cursor - fmin;
            for f in fr {
                let mut g = f.clone();
                g.t += shift;
                out.push(g);
            }
            cursor += (fmax - fmin) + 0.001;
        }
    } else {
        for (_, fr) in &a.pb_files {
            out.extend(fr.iter().cloned());
        }
    }
    out.sort_by(|x, y| x.t.partial_cmp(&y.t).unwrap_or(std::cmp::Ordering::Equal));
    a.pb_raw = out;

    let en = a.lang_en;
    let total = a.pb_raw.len();
    let names: Vec<String> = a.pb_files.iter().map(|(n, _)| n.clone()).collect();
    let fname = match names.len() {
        0 => {
            if en {
                "(no file selected)".to_string()
            } else {
                "(未选择文件)".to_string()
            }
        }
        1 => {
            if en {
                format!("{} ({total} frames)", names[0])
            } else {
                format!("{} ({total} 帧)", names[0])
            }
        }
        n => {
            if en {
                format!("{n} files: {} ({total} frames)", names.join(", "))
            } else {
                format!("{n} 个文件: {} ({total} 帧)", names.join(", "))
            }
        }
    };
    w.set_file_name(fname.into());

    let mut chans: Vec<u8> = a.pb_raw.iter().map(|f| f.ch).collect();
    chans.sort_unstable();
    chans.dedup();
    let ctxt = if chans.is_empty() {
        "-".to_string()
    } else {
        chans
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    w.set_src_channels(ctxt.into());

    let rows: Vec<PbFileRow> = a
        .pb_files
        .iter()
        .map(|(n, fr)| PbFileRow {
            name: n.clone().into(),
            count: if en {
                format!("{} frames", fr.len())
            } else {
                format!("{} 帧", fr.len())
            }
            .into(),
        })
        .collect();
    w.set_pb_files(ModelRc::from(Rc::new(VecModel::from(rows))));

    pb_build_and_load(a, w);
}

pub(super) fn pb_build_and_load(a: &App, w: &PlaybackWindow) {
    let lo = parse_hex_u32(&w.get_id_lo()).unwrap_or(0);
    let hi = parse_hex_u32(&w.get_id_hi()).unwrap_or(u32::MAX);
    let ss = w
        .get_seg_start()
        .to_string()
        .trim()
        .parse::<f64>()
        .unwrap_or(f64::MIN);
    let se = w
        .get_seg_end()
        .to_string()
        .trim()
        .parse::<f64>()
        .unwrap_or(f64::MAX);
    let map = parse_channel_map(&w.get_channel_map());
    let frames: Vec<CanFrame> = a
        .pb_raw
        .iter()
        .filter(|f| f.id >= lo && f.id <= hi && f.t >= ss && f.t <= se)
        .filter_map(|f| {
            let dst = map.get(&f.ch).copied().unwrap_or(f.ch);
            if dst == 0 {
                return None;
            }
            let mut g = f.clone();
            g.ch = dst;
            Some(g)
        })
        .collect();
    let _ = a.cmd.send(Cmd::PlaybackLoad(frames));
}

pub(super) fn parse_channel_map(s: &slint::SharedString) -> std::collections::HashMap<u8, u8> {
    let mut m = std::collections::HashMap::new();
    for tok in s.as_str().split(',') {
        let tok = tok.trim();
        if let Some((a, b)) = tok.split_once(':')
            && let (Ok(src), Ok(dst)) = (a.trim().parse::<u8>(), b.trim().parse::<u8>())
        {
            m.insert(src, dst);
        }
    }
    m
}

pub(super) fn parse_hex_u32(s: &slint::SharedString) -> Option<u32> {
    let t = s.to_string();
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    let t = t.trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(t, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::playback_online_mode;

    #[test]
    fn online_playback_falls_back_to_offline_without_hardware() {
        assert!(!playback_online_mode(true, false));
        assert!(playback_online_mode(true, true));
        assert!(!playback_online_mode(false, true));
    }
}
