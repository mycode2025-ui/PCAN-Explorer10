//! channel management responsibilities extracted from src/main.rs.
use super::*;

pub(super) fn default_channel() -> DeviceConfig {
    DeviceConfig {
        sw_channel: 1,
        is_fd: false,
        device_type: "Virtual".into(),
        hardware_label: String::new(),
        hardware_id: String::new(),
        device_index: 0,
        channel_index: 0,
        baud: "500K".into(),
        data_baud: "2M".into(),
        custom_bitrate: String::new(),
        termination: false,
        listen_only: false,
        fd_non_iso: false,
        net_server: true,
        ip: "192.168.0.178".into(),
        port: "8000".into(),
    }
}

pub(super) fn renumber_channel_slice(channels: &mut [DeviceConfig]) {
    for (i, c) in channels.iter_mut().enumerate() {
        c.sw_channel = (i + 1) as u8;
    }
}

pub(super) fn channel_configs(a: &App) -> &[DeviceConfig] {
    a.channel_edit
        .as_ref()
        .map(|session| session.channels.as_slice())
        .unwrap_or(a.channels.as_slice())
}

pub(super) fn channel_selected(a: &App) -> i32 {
    a.channel_edit
        .as_ref()
        .map(|session| session.selected)
        .unwrap_or(a.channel_sel)
}

pub(super) fn normalized_channel_selection(a: &App) -> i32 {
    let count = channel_configs(a).len() as i32;
    if count == 0 {
        -1
    } else {
        channel_selected(a).clamp(0, count - 1)
    }
}

pub(super) fn ensure_channel_edit_session(a: &mut App) {
    if a.channel_edit.is_none() {
        a.channel_edit = Some(ChannelEditSession {
            channels: a.channels.clone(),
            selected: a.channel_sel,
            dirty: false,
        });
    }
}

pub(super) fn set_chan_form(w: &ChannelConfigWindow, c: &DeviceConfig, a: &App) {
    let device_upper = c.device_type.trim().to_ascii_uppercase();
    let detected_pcan = a.pcan_devices.iter().find(|hardware| {
        (!c.hardware_id.is_empty() && c.hardware_id == pcan_hardware_id(hardware))
            || (device_upper == "PCAN" && hardware.channel_index == c.channel_index)
    });
    let detected_zcan = a.zcan_devices.iter().find(|hardware| {
        (!c.hardware_id.is_empty() && c.hardware_id == zcan_hardware_id(hardware))
            || (hardware.device_type.eq_ignore_ascii_case(&c.device_type)
                && hardware.device_index == c.device_index
                && hardware.channel_index == c.channel_index)
    });
    let is_zlg_fd = device_upper.contains("USBCANFD");
    let is_network_fd = device_upper.contains("CANFDNET") || device_upper.contains("CANFDWIFI");
    let supports_fd = detected_pcan
        .map(|hardware| hardware.fd_capable)
        .or_else(|| detected_zcan.map(|hardware| hardware.fd_capable))
        .unwrap_or(is_zlg_fd || is_network_fd || c.is_fd);
    let supports_termination = is_zlg_fd;
    let supports_listen_only = device_upper != "PCAN"
        && (detected_zcan.is_some()
            || is_zlg_fd
            || device_upper.contains("USBCAN")
            || matches!(device_upper.as_str(), "GCAN" | "ZHCX" | "ZHCXCAN"));
    let supports_non_iso = supports_fd && device_upper != "PCAN";
    let identity = if !c.hardware_id.is_empty() {
        c.hardware_id.clone()
    } else if let Some(hardware) = detected_pcan {
        pcan_hardware_id(hardware)
    } else if let Some(hardware) = detected_zcan {
        zcan_hardware_id(hardware)
    } else {
        String::new()
    };
    let state = if detected_pcan.is_some() || detected_zcan.is_some() {
        if a.lang_en {
            "Detected and matched"
        } else {
            "已检测并匹配"
        }
    } else if identity.is_empty() {
        if a.lang_en {
            "Manual mapping; verify indices before connecting"
        } else {
            "手动映射，连接前请核对索引"
        }
    } else if a.lang_en {
        "Saved hardware is currently offline"
    } else {
        "已保存的硬件当前不在线"
    };
    let arbitration = if device_upper == "PCAN" && supports_fd {
        vec!["1M", "800K", "500K", "250K", "125K"]
    } else if device_upper == "PCAN" {
        vec!["1M", "500K", "250K", "125K"]
    } else if supports_fd {
        vec!["1M", "800K", "500K", "250K", "125K"]
    } else {
        vec![
            "1M", "800K", "500K", "250K", "125K", "100K", "50K", "20K", "10K", "5K",
        ]
    };
    let data_rates = vec!["8M", "5M", "4M", "2M", "1M", "800K", "500K", "250K", "125K"];
    w.set_is_fd(c.is_fd);
    w.set_device_type(c.device_type.clone().into());
    w.set_hardware_label(c.hardware_label.clone().into());
    w.set_device_index(c.device_index.to_string().into());
    w.set_channel_index((c.channel_index + 1).to_string().into());
    w.set_baud(c.baud.clone().into());
    w.set_data_baud(c.data_baud.clone().into());
    w.set_custom_bitrate(c.custom_bitrate.clone().into());
    w.set_termination(c.termination);
    w.set_listen_only(c.listen_only);
    w.set_fd_non_iso(c.fd_non_iso);
    w.set_manual_mode(c.hardware_id.is_empty());
    w.set_supports_fd(supports_fd);
    w.set_supports_termination(supports_termination);
    w.set_supports_listen_only(supports_listen_only);
    w.set_supports_non_iso(supports_non_iso);
    w.set_arb_baud_options(ModelRc::from(Rc::new(VecModel::from(
        arbitration
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    ))));
    w.set_data_baud_options(ModelRc::from(Rc::new(VecModel::from(
        data_rates
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    ))));
    w.set_hardware_identity(identity.into());
    w.set_device_state(state.into());
    w.set_net_server(c.net_server);
    w.set_ip(c.ip.clone().into());
    w.set_port(c.port.clone().into());
}

pub(super) fn chan_list_strings(a: &App) -> Vec<SharedString> {
    channel_configs(a)
        .iter()
        .map(|c| {
            let label = c.hardware_label.trim();
            let label = if label.is_empty() { "Unnamed" } else { label };
            let proto = if c.is_fd { "CAN FD" } else { "CAN" };
            format!("CAN{}  {}  {}", c.sw_channel, label, proto).into()
        })
        .collect()
}

pub(super) fn chan_detail_strings(a: &App) -> Vec<SharedString> {
    channel_configs(a)
        .iter()
        .map(|c| {
            let dev = c.device_type.trim();
            let bus = if dev.eq_ignore_ascii_case("PCAN") {
                if let Some(hw) = a
                    .pcan_devices
                    .iter()
                    .find(|hw| hw.channel_index == c.channel_index)
                {
                    format!(
                        "{} {}: Device ID {:X}h",
                        hw.channel_name, hw.device_name, hw.device_id
                    )
                } else {
                    format!("PCAN_USBBUS{} not detected", c.channel_index + 1)
                }
            } else if dev.to_ascii_uppercase().contains("NET")
                || dev.to_ascii_uppercase().contains("WIFI")
            {
                format!("{}:{}", c.ip, c.port)
            } else {
                format!("dev{} CAN{}", c.device_index, c.channel_index + 1)
            };
            let pcan_cap = if dev.eq_ignore_ascii_case("PCAN") {
                a.pcan_devices
                    .iter()
                    .find(|hw| hw.channel_index == c.channel_index)
                    .map(|hw| hw.fd_capable)
            } else {
                None
            };
            let proto = if c.is_fd {
                let mut s = format!("CANFD {}/{}", c.baud, c.data_baud);
                if matches!(pcan_cap, Some(false)) {
                    s.push_str(" !");
                }
                s
            } else {
                format!("CAN {}", c.baud)
            };
            format!("{}  {}  {}", dev, bus, proto).into()
        })
        .collect()
}

pub(super) struct HardwareDisplayRows {
    pub(super) titles: Vec<SharedString>,
    pub(super) details: Vec<SharedString>,
    pub(super) added: Vec<bool>,
    pub(super) groups: Vec<bool>,
    pub(super) sources: Vec<i32>,
    pub(super) enabled: Vec<bool>,
}

impl HardwareDisplayRows {
    pub(super) fn push(
        &mut self,
        title: String,
        detail: String,
        added: bool,
        group: bool,
        source: i32,
        enabled: bool,
    ) {
        self.titles.push(title.into());
        self.details.push(detail.into());
        self.added.push(added);
        self.groups.push(group);
        self.sources.push(source);
        self.enabled.push(enabled);
    }
}

pub(super) fn pcan_hardware_id(hw: &can::PcanChannelInfo) -> String {
    format!("PCAN:{:08X}:{}", hw.device_id, hw.channel_index)
}

pub(super) fn zcan_hardware_id(hw: &can::ZcanUsbChannelInfo) -> String {
    let identity = if hw.serial_number.trim().is_empty() {
        format!("DEV{}", hw.device_index)
    } else {
        hw.serial_number.trim().to_ascii_uppercase()
    };
    format!(
        "{}:{}:{}",
        hw.device_type.trim().to_ascii_uppercase(),
        identity,
        hw.channel_index
    )
}

pub(super) fn hardware_display_rows(
    devices: &[can::PcanChannelInfo],
    zcan_devices: &[can::ZcanUsbChannelInfo],
    channels: &[DeviceConfig],
    english: bool,
) -> HardwareDisplayRows {
    let mut result = HardwareDisplayRows {
        titles: Vec::new(),
        details: Vec::new(),
        added: Vec::new(),
        groups: Vec::new(),
        sources: Vec::new(),
        enabled: Vec::new(),
    };
    let mut pcan_groups =
        std::collections::BTreeMap::<(u32, String), Vec<(usize, &can::PcanChannelInfo)>>::new();
    for (index, hw) in devices.iter().enumerate() {
        pcan_groups
            .entry((hw.device_id, hw.device_name.clone()))
            .or_default()
            .push((index, hw));
    }
    for ((device_id, device_name), rows) in pcan_groups {
        result.push(
            format!("PEAK  {device_name}"),
            format!("Device ID {device_id:08X}h · {} channel(s)", rows.len()),
            false,
            true,
            -1,
            false,
        );
        for (source, hw) in rows {
            let stable_id = pcan_hardware_id(hw);
            let is_added = channels.iter().any(|channel| {
                (!channel.hardware_id.is_empty() && channel.hardware_id == stable_id)
                    || (channel.device_type.eq_ignore_ascii_case("PCAN")
                        && channel.channel_index == hw.channel_index)
            });
            let condition = match hw.channel_condition {
                1 => {
                    if english {
                        "available"
                    } else {
                        "可用"
                    }
                }
                2 | 4 => {
                    if english {
                        "in use"
                    } else {
                        "已占用"
                    }
                }
                _ => {
                    if english {
                        "unavailable"
                    } else {
                        "不可用"
                    }
                }
            };
            result.push(
                format!("↳ {}", hw.channel_name),
                format!(
                    "{} · {}",
                    if hw.fd_capable {
                        "CAN FD"
                    } else {
                        "Classical CAN"
                    },
                    condition
                ),
                is_added,
                false,
                source as i32,
                hw.channel_condition == 1 || is_added,
            );
        }
    }

    let offset = devices.len();
    let mut zcan_groups =
        std::collections::BTreeMap::<String, Vec<(usize, &can::ZcanUsbChannelInfo)>>::new();
    for (index, hw) in zcan_devices.iter().enumerate() {
        let serial = if hw.serial_number.trim().is_empty() {
            format!("dev{}", hw.device_index)
        } else {
            format!("SN {}", hw.serial_number.trim())
        };
        let key = format!("{}|{}|{}", hw.device_type, serial, hw.hardware_label);
        zcan_groups.entry(key).or_default().push((index, hw));
    }
    for (_key, rows) in zcan_groups {
        let first = rows[0].1;
        let vendor = match first.device_type.to_ascii_uppercase().as_str() {
            "GCAN" => "GCAN",
            "ZHCX" | "ZHCXCAN" => "ZHCX",
            _ => "ZLG",
        };
        let identity = if first.serial_number.trim().is_empty() {
            format!("dev{}", first.device_index)
        } else {
            format!("SN {}", first.serial_number.trim())
        };
        result.push(
            format!("{vendor}  {}", first.hardware_label),
            format!("{identity} · {} channel(s)", rows.len()),
            false,
            true,
            -1,
            false,
        );
        for (source, hw) in rows {
            let stable_id = zcan_hardware_id(hw);
            let is_added = channels.iter().any(|channel| {
                (!channel.hardware_id.is_empty() && channel.hardware_id == stable_id)
                    || (channel.device_type.eq_ignore_ascii_case(&hw.device_type)
                        && channel.device_index == hw.device_index
                        && channel.channel_index == hw.channel_index)
            });
            result.push(
                format!("↳ CAN{}", hw.channel_index + 1),
                format!(
                    "{} · dev{} ch{}",
                    if hw.fd_capable {
                        "CAN FD"
                    } else {
                        "Classical CAN"
                    },
                    hw.device_index,
                    hw.channel_index
                ),
                is_added,
                false,
                (offset + source) as i32,
                true,
            );
        }
    }

    if result.titles.is_empty() {
        result.titles.push(
            if english {
                "No CAN hardware detected"
            } else {
                "未发现 CAN 硬件"
            }
            .into(),
        );
        result.details.push(
            if english {
                "Check USB connection, driver installation, and device occupancy"
            } else {
                "请检查 USB、驱动安装以及设备是否被其他软件占用"
            }
            .into(),
        );
        result.added.push(false);
        result.groups.push(true);
        result.sources.push(-1);
        result.enabled.push(false);
    }
    result
}

pub(super) fn refresh_and_reconcile_pcan(a: &mut App) {
    reconcile_stable_hardware(a);
    let configured = a
        .channels
        .iter()
        .enumerate()
        .filter(|(_, channel)| channel.device_type.eq_ignore_ascii_case("PCAN"))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if configured.len() != 1 || a.pcan_devices.len() != 1 {
        return;
    }
    let config_index = configured[0];
    let hardware = a.pcan_devices[0].clone();
    let configured_index = a.channels[config_index].channel_index;
    if configured_index == hardware.channel_index {
        return;
    }
    a.channels[config_index].channel_index = hardware.channel_index;
    if a.channels[config_index].hardware_label.trim().is_empty() {
        a.channels[config_index].hardware_label = hardware.device_name.clone();
    }
    a.log(format!(
        "PCAN 硬件通道已自动校正: PCAN_USBBUS{} → {}",
        configured_index + 1,
        hardware.channel_name
    ));
}

pub(super) fn reconcile_stable_hardware(a: &mut App) {
    for channel in &mut a.channels {
        if channel.hardware_id.is_empty() {
            continue;
        }
        if let Some(hardware) = a
            .pcan_devices
            .iter()
            .find(|hardware| pcan_hardware_id(hardware) == channel.hardware_id)
        {
            channel.device_type = "PCAN".into();
            channel.device_index = 0;
            channel.channel_index = hardware.channel_index;
            if channel.hardware_label.trim().is_empty() {
                channel.hardware_label = hardware.device_name.clone();
            }
            continue;
        }
        if let Some(hardware) = a
            .zcan_devices
            .iter()
            .find(|hardware| zcan_hardware_id(hardware) == channel.hardware_id)
        {
            channel.device_type = hardware.device_type.clone();
            channel.device_index = hardware.device_index;
            channel.channel_index = hardware.channel_index;
            if channel.hardware_label.trim().is_empty() {
                channel.hardware_label = hardware.hardware_label.clone();
            }
        }
    }
    if let Some(session) = a.channel_edit.as_mut() {
        for channel in &mut session.channels {
            if channel.hardware_id.is_empty() {
                continue;
            }
            if let Some(hardware) = a
                .pcan_devices
                .iter()
                .find(|hardware| pcan_hardware_id(hardware) == channel.hardware_id)
            {
                channel.device_type = "PCAN".into();
                channel.device_index = 0;
                channel.channel_index = hardware.channel_index;
            } else if let Some(hardware) = a
                .zcan_devices
                .iter()
                .find(|hardware| zcan_hardware_id(hardware) == channel.hardware_id)
            {
                channel.device_type = hardware.device_type.clone();
                channel.device_index = hardware.device_index;
                channel.channel_index = hardware.channel_index;
            }
        }
    }
}

pub(super) fn scan_attached_hardware(a: &mut App) -> bool {
    const MIN_SCAN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
    let now = std::time::Instant::now();
    if a.last_hardware_scan
        .is_some_and(|last| now.saturating_duration_since(last) < MIN_SCAN_INTERVAL)
    {
        return false;
    }

    // Some vendor USB-CAN drivers are not re-entrant and can corrupt their
    // internal state when OpenDevice/CloseDevice is called repeatedly in a
    // tight loop. Mark the scan before entering the DLL so re-entrant UI
    // callbacks cannot start a second scan.
    a.last_hardware_scan = Some(now);
    if a.hardware_scan_in_progress {
        return false;
    }
    a.hardware_scan_in_progress = true;
    a.hardware_scan_status = if a.lang_en {
        "Scanning PEAK, ZLG, GCAN and ZHCX drivers...".into()
    } else {
        "正在扫描 PEAK、ZLG、GCAN 与 ZHCX 驱动...".into()
    };
    let worker = a.worker_tx.clone();
    let retained_zcan = a.connected.then(|| a.zcan_devices.clone());
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let pcan = can::pcan_attached_channels();
        let zcan = retained_zcan.unwrap_or_else(can::zcan_attached_channels);
        let _ = worker.send(WorkerEvent::HardwareScanned {
            pcan,
            zcan,
            elapsed_ms: started.elapsed().as_millis(),
        });
    });
    true
}

pub(super) fn refresh_channel_window_lists(w: &ChannelConfigWindow, a: &App) {
    w.set_chan_sel(normalized_channel_selection(a));
    w.set_channels(ModelRc::from(Rc::new(VecModel::from(chan_list_strings(a)))));
    w.set_channel_details(ModelRc::from(Rc::new(VecModel::from(chan_detail_strings(
        a,
    )))));
    let hardware = hardware_display_rows(
        &a.pcan_devices,
        &a.zcan_devices,
        channel_configs(a),
        a.lang_en,
    );
    w.set_pcan_hardware(ModelRc::from(Rc::new(VecModel::from(hardware.titles))));
    w.set_pcan_hardware_details(ModelRc::from(Rc::new(VecModel::from(hardware.details))));
    w.set_pcan_hardware_added(ModelRc::from(Rc::new(VecModel::from(hardware.added))));
    w.set_pcan_hardware_group(ModelRc::from(Rc::new(VecModel::from(hardware.groups))));
    w.set_pcan_hardware_source(ModelRc::from(Rc::new(VecModel::from(hardware.sources))));
    w.set_pcan_hardware_enabled(ModelRc::from(Rc::new(VecModel::from(hardware.enabled))));
    w.set_scan_in_progress(a.hardware_scan_in_progress);
    w.set_scan_status(a.hardware_scan_status.clone().into());
    w.set_connecting(a.channel_connect_pending);
    w.set_config_dirty(a.channel_edit.as_ref().is_some_and(|session| session.dirty));
}
