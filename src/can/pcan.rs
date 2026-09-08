//! pcan responsibilities extracted from src/can.rs.
use super::*;

#[allow(non_camel_case_types)]
pub(super) mod pcan_ffi {
    pub type FnInit = unsafe extern "system" fn(u16, u16, u8, u32, u16) -> u32;
    pub type FnUninit = unsafe extern "system" fn(u16) -> u32;
    pub type FnRead = unsafe extern "system" fn(u16, *mut TPCANMsg, *mut TPCANTimestamp) -> u32;
    pub type FnWrite = unsafe extern "system" fn(u16, *const TPCANMsg) -> u32;
    pub type FnGetValue = unsafe extern "system" fn(u16, u8, *mut std::ffi::c_void, u32) -> u32;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct TPCANMsg {
        pub id: u32,
        pub msgtype: u8,
        pub len: u8,
        pub data: [u8; 8],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct TPCANTimestamp {
        pub millis: u32,
        pub millis_overflow: u16,
        pub micros: u16,
    }

    pub const PCAN_NONEBUS: u16 = 0x00;
    pub const PCAN_BAUD_125K: u16 = 0x031C;
    pub const PCAN_BAUD_250K: u16 = 0x011C;
    pub const PCAN_BAUD_500K: u16 = 0x001C;
    pub const PCAN_BAUD_1M: u16 = 0x0014;
    pub const PCAN_ERROR_OK: u32 = 0x0000_0000;
    pub const PCAN_ERROR_QRCVEMPTY: u32 = 0x0000_0020;
    pub const PCAN_ERROR_OVERRUN: u32 = 0x0000_0002;
    pub const PCAN_ERROR_BUSLIGHT: u32 = 0x0000_0004;
    pub const PCAN_ERROR_BUSHEAVY: u32 = 0x0000_0008;
    pub const PCAN_ERROR_BUSOFF: u32 = 0x0000_0010;
    pub const PCAN_ERROR_QOVERRUN: u32 = 0x0000_0040;
    pub const PCAN_ERROR_NODRIVER: u32 = 0x0000_0200;
    pub const PCAN_ERROR_ILLHW: u32 = 0x0000_1400;
    pub const PCAN_ERROR_INITIALIZE: u32 = 0x0400_0000;
    pub const PCAN_ATTACHED_CHANNELS_COUNT: u8 = 0x2A;
    pub const PCAN_ATTACHED_CHANNELS: u8 = 0x2B;
    pub const PCAN_FEATURE_FD_CAPABLE: u32 = 0x01;

    pub const MSGTYPE_STANDARD: u8 = 0x00;
    pub const MSGTYPE_RTR: u8 = 0x01;
    pub const MSGTYPE_EXTENDED: u8 = 0x02;
    pub const MSGTYPE_FD: u8 = 0x04;
    pub const MSGTYPE_BRS: u8 = 0x08;
    pub const MSGTYPE_ERRFRAME: u8 = 0x40;
    pub const MSGTYPE_STATUS: u8 = 0x80;

    pub type FnInitFd = unsafe extern "system" fn(u16, *const std::os::raw::c_char) -> u32;
    pub type FnWriteFd = unsafe extern "system" fn(u16, *const TPCANMsgFD) -> u32;
    pub type FnReadFd = unsafe extern "system" fn(u16, *mut TPCANMsgFD, *mut u64) -> u32;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct TPCANMsgFD {
        pub id: u32,
        pub msgtype: u8,
        pub dlc: u8,
        pub data: [u8; 64],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct TPCANChannelInformation {
        pub channel_handle: u16,
        pub device_type: u8,
        pub controller_number: u8,
        pub device_features: u32,
        pub device_name: [std::os::raw::c_char; 33],
        pub device_id: u32,
        pub channel_condition: u32,
    }
}

#[derive(Clone, Debug)]
pub struct PcanChannelInfo {
    pub channel_index: u32,
    pub channel_name: String,
    pub device_name: String,
    pub device_id: u32,
    pub fd_capable: bool,
    pub channel_condition: u32,
}

#[derive(Clone, Debug)]
pub struct ZcanUsbChannelInfo {
    pub device_type: String,
    pub hardware_label: String,
    pub serial_number: String,
    pub device_index: u32,
    pub channel_index: u32,
    pub fd_capable: bool,
}

pub(super) fn pcan_channel_index(handle: u16) -> Option<u32> {
    match handle {
        0x51..=0x58 => Some((handle - 0x51) as u32),
        0x509..=0x510 => Some((handle - 0x509 + 8) as u32),
        _ => None,
    }
}

pub(super) fn pcan_channel_name(index: u32) -> String {
    format!("PCAN_USBBUS{}", index + 1)
}

pub(super) fn pcan_usb_channel(index: u32) -> Option<u16> {
    match index {
        0 => Some(0x51),
        1 => Some(0x52),
        2 => Some(0x53),
        3 => Some(0x54),
        4 => Some(0x55),
        5 => Some(0x56),
        6 => Some(0x57),
        7 => Some(0x58),
        8 => Some(0x509),
        9 => Some(0x50A),
        10 => Some(0x50B),
        11 => Some(0x50C),
        12 => Some(0x50D),
        13 => Some(0x50E),
        14 => Some(0x50F),
        15 => Some(0x510),
        _ => None,
    }
}

pub(super) fn pcan_device_name(raw: &[std::os::raw::c_char; 33]) -> String {
    let bytes: Vec<u8> = raw
        .iter()
        .copied()
        .take_while(|&c| c != 0)
        .map(|c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).trim().to_string()
}

pub fn pcan_attached_channels() -> Vec<PcanChannelInfo> {
    use pcan_ffi::*;

    let Ok(lib) = (unsafe { libloading::Library::new("PCANBasic.dll") }) else {
        return Vec::new();
    };
    let Ok(get_value) = (unsafe { lib.get::<FnGetValue>(b"CAN_GetValue\0") }) else {
        return Vec::new();
    };

    let mut count = 0u32;
    let status = unsafe {
        get_value(
            PCAN_NONEBUS,
            PCAN_ATTACHED_CHANNELS_COUNT,
            (&mut count as *mut u32).cast::<std::ffi::c_void>(),
            std::mem::size_of::<u32>() as u32,
        )
    };
    if status != PCAN_ERROR_OK || count == 0 {
        return Vec::new();
    }

    let empty = TPCANChannelInformation {
        channel_handle: 0,
        device_type: 0,
        controller_number: 0,
        device_features: 0,
        device_name: [0; 33],
        device_id: 0,
        channel_condition: 0,
    };
    let mut raw = vec![empty; count as usize];
    let status = unsafe {
        get_value(
            PCAN_NONEBUS,
            PCAN_ATTACHED_CHANNELS,
            raw.as_mut_ptr().cast::<std::ffi::c_void>(),
            (raw.len() * std::mem::size_of::<TPCANChannelInformation>()) as u32,
        )
    };
    if status != PCAN_ERROR_OK {
        return Vec::new();
    }

    raw.into_iter()
        .filter_map(|info| {
            let channel_index = pcan_channel_index(info.channel_handle)?;
            let mut device_name = pcan_device_name(&info.device_name);
            let fd_capable = info.device_features & PCAN_FEATURE_FD_CAPABLE != 0;
            if device_name.is_empty() {
                device_name = if fd_capable {
                    "PCAN-USB FD".to_string()
                } else {
                    "PCAN-USB".to_string()
                };
            }
            Some(PcanChannelInfo {
                channel_index,
                channel_name: pcan_channel_name(channel_index),
                device_name,
                device_id: info.device_id,
                fd_capable,
                channel_condition: info.channel_condition,
            })
        })
        .collect()
}

pub(super) fn fd_len_to_dlc(len: usize) -> u8 {
    match len {
        0..=8 => len as u8,
        9..=12 => 9,
        13..=16 => 10,
        17..=20 => 11,
        21..=24 => 12,
        25..=32 => 13,
        33..=48 => 14,
        _ => 15,
    }
}

pub(super) fn fd_dlc_to_len(dlc: u8) -> usize {
    match dlc & 0x0F {
        n @ 0..=8 => n as usize,
        9 => 12,
        10 => 16,
        11 => 20,
        12 => 24,
        13 => 32,
        14 => 48,
        _ => 64,
    }
}

pub(super) fn pcan_fd_bitrate(arb: &str, data: &str) -> Result<String, String> {
    let a = normalize_baud(arb);
    if a.contains("F_CLOCK") || a.contains("NOM_BRP") {
        return Ok(arb.to_string());
    }
    let nom = match a.as_str() {
        "1M" | "1000K" => "nom_brp=2,nom_tseg1=31,nom_tseg2=8,nom_sjw=8",
        "800K" => "nom_brp=5,nom_tseg1=15,nom_tseg2=4,nom_sjw=4",
        "500K" => "nom_brp=2,nom_tseg1=63,nom_tseg2=16,nom_sjw=16",
        "250K" => "nom_brp=4,nom_tseg1=63,nom_tseg2=16,nom_sjw=16",
        "125K" => "nom_brp=8,nom_tseg1=63,nom_tseg2=16,nom_sjw=16",
        other => {
            return Err(format!(
                "CAN FD 不支持的仲裁速率: {other}（支持 1M/500K/250K/125K，或直接填完整 f_clock 串）"
            ));
        }
    };
    let dat = match normalize_baud(data).as_str() {
        "8M" | "8000K" => "data_brp=1,data_tseg1=7,data_tseg2=2,data_sjw=2",
        "5M" | "5000K" => "data_brp=1,data_tseg1=12,data_tseg2=3,data_sjw=3",
        "4M" | "4000K" => "data_brp=1,data_tseg1=15,data_tseg2=4,data_sjw=4",
        "2M" | "2000K" => "data_brp=2,data_tseg1=15,data_tseg2=4,data_sjw=4",
        "1M" | "1000K" => "data_brp=4,data_tseg1=15,data_tseg2=4,data_sjw=4",
        "800K" => "data_brp=5,data_tseg1=15,data_tseg2=4,data_sjw=4",
        "500K" => "data_brp=8,data_tseg1=15,data_tseg2=4,data_sjw=4",
        "250K" => "data_brp=16,data_tseg1=15,data_tseg2=4,data_sjw=4",
        "125K" => "data_brp=32,data_tseg1=15,data_tseg2=4,data_sjw=4",
        other => {
            return Err(format!(
                "CAN FD 不支持的数据速率: {other}（支持 8M/5M/4M/2M/1M/500K，或直接填完整 f_clock 串）"
            ));
        }
    };
    Ok(format!("f_clock=80000000,{nom},{dat}"))
}

pub(super) fn pcan_poll_error(status: u32) -> PollReport {
    use pcan_ffi::*;
    let receive_overruns = u64::from(status & (PCAN_ERROR_QOVERRUN | PCAN_ERROR_OVERRUN) != 0);
    let bus_state = if status & PCAN_ERROR_BUSOFF != 0 {
        " bus-off"
    } else if status & PCAN_ERROR_BUSHEAVY != 0 {
        " bus-heavy"
    } else if status & PCAN_ERROR_BUSLIGHT != 0 {
        " bus-light"
    } else {
        ""
    };
    PollReport {
        receive_overruns,
        driver_errors: 1,
        connection_lost: status == PCAN_ERROR_ILLHW
            || status & PCAN_ERROR_NODRIVER != 0
            || status & PCAN_ERROR_INITIALIZE != 0,
        message: Some(format!("PCAN 接收状态 0x{status:08X}{bus_state}")),
    }
}

pub struct PcanBus {
    pub(super) lib: libloading::Library,
    pub(super) channel: u16,
    pub(super) is_fd: bool,
    pub(super) start: Instant,
    pub(super) timestamp: HardwareTimebase,
    pub(super) name: String,
}

impl PcanBus {
    pub fn open(start: Instant) -> Result<Self, String> {
        Self::open_config(start, 0, "500K")
    }

    pub fn open_config(start: Instant, channel_index: u32, baud: &str) -> Result<Self, String> {
        use pcan_ffi::*;
        let Some(channel) = pcan_usb_channel(channel_index) else {
            return Err(format!(
                "PCAN only supports configured USB channel index 0..15, got {channel_index}"
            ));
        };
        let baud_code = match normalize_baud(baud).as_str() {
            "125K" => PCAN_BAUD_125K,
            "250K" => PCAN_BAUD_250K,
            "500K" => PCAN_BAUD_500K,
            "1000K" | "1M" => PCAN_BAUD_1M,
            other => return Err(format!("Unsupported PCAN baud rate: {other}")),
        };
        unsafe {
            let lib = libloading::Library::new("PCANBasic.dll")
                .map_err(|e| format!("加载 PCANBasic.dll 失败: {e}"))?;
            let init: libloading::Symbol<FnInit> = lib
                .get(b"CAN_Initialize\0")
                .map_err(|e| format!("找不到 CAN_Initialize: {e}"))?;
            let status = init(channel, baud_code, 0, 0, 0);
            drop(init);
            let use_fd_api = if status == PCAN_ERROR_OK {
                false
            } else if status == PCAN_ERROR_ILLHW {
                // Some PCAN-USB FD driver versions expose an attached channel
                // but reject the classic initialization entry point.  The FD
                // API can still run that channel at the requested arbitration
                // bitrate and carry ordinary CAN 2.0 frames (without the FD
                // message flag), so fall back transparently.
                let bitrate = pcan_fd_bitrate(baud, baud)?;
                let bitrate_c = std::ffi::CString::new(bitrate.clone()).unwrap();
                let init_fd: libloading::Symbol<FnInitFd> = lib
                    .get(b"CAN_InitializeFD\0")
                    .map_err(|e| format!("CAN_InitializeFD 未找到: {e}"))?;
                let fd_status = init_fd(channel, bitrate_c.as_ptr());
                drop(init_fd);
                if fd_status != PCAN_ERROR_OK {
                    return Err(format!(
                        "CAN_Initialize 失败 0x{status:08X}，FD API 回退也失败 0x{fd_status:08X}"
                    ));
                }
                true
            } else {
                return Err(format!(
                    "CAN_Initialize 失败, status=0x{status:08X}（设备未连接、通道被其他程序占用或 PEAK 驱动不可用；请关闭其他 CAN 工具后重试）"
                ));
            };
            Ok(Self {
                lib,
                channel,
                is_fd: use_fd_api,
                start,
                timestamp: HardwareTimebase::new(1e-6, None),
                name: format!("PCAN_USBBUS{} @{}", channel_index + 1, normalize_baud(baud)),
            })
        }
    }

    pub fn open_cfg(start: Instant, cfg: &DeviceConfig) -> Result<Self, String> {
        use pcan_ffi::*;
        if !cfg.is_fd {
            // On PCAN-USB FD hardware, the legacy CAN_Initialize timing
            // presets can use a different sample point from modern ZLG
            // adapters at the same nominal bitrate. Initialize the FD-capable
            // controller through CAN_InitializeFD with equal nominal/data
            // rates, while still transmitting ordinary CAN 2.0 frames unless
            // the frame itself carries MSGTYPE_FD.
            let fd_capable = pcan_attached_channels()
                .into_iter()
                .any(|channel| channel.channel_index == cfg.channel_index && channel.fd_capable);
            if fd_capable {
                let mut classic_via_fd = cfg.clone();
                classic_via_fd.is_fd = true;
                classic_via_fd.data_baud = cfg.baud.clone();
                classic_via_fd.custom_bitrate.clear();
                let mut bus = Self::open_cfg(start, &classic_via_fd)?;
                bus.name = format!(
                    "PCAN_USBBUS{} @{} (FD API, Classical CAN)",
                    cfg.channel_index + 1,
                    normalize_baud(&cfg.baud)
                );
                return Ok(bus);
            }
            return Self::open_config(start, cfg.channel_index, &cfg.baud);
        }
        let Some(channel) = pcan_usb_channel(cfg.channel_index) else {
            return Err(format!(
                "PCAN only supports configured USB channel index 0..15, got {}",
                cfg.channel_index
            ));
        };
        let bitrate = if cfg.custom_bitrate.trim().is_empty() {
            pcan_fd_bitrate(&cfg.baud, &cfg.data_baud)?
        } else {
            pcan_fd_bitrate(cfg.custom_bitrate.trim(), &cfg.data_baud)?
        };
        let bitrate_c = std::ffi::CString::new(bitrate.clone()).unwrap();
        unsafe {
            let lib = libloading::Library::new("PCANBasic.dll")
                .map_err(|e| format!("加载 PCANBasic.dll 失败: {e}"))?;
            let init_fd: libloading::Symbol<FnInitFd> = lib
                .get(b"CAN_InitializeFD\0")
                .map_err(|e| format!("找不到 CAN_InitializeFD（驱动太旧?）: {e}"))?;
            let status = init_fd(channel, bitrate_c.as_ptr());
            if status != PCAN_ERROR_OK {
                return Err(format!(
                    "CAN_InitializeFD 失败, status=0x{status:08X}（检查卡是否支持 FD/时钟，比特率串: {bitrate}）"
                ));
            }
            drop(init_fd);
            Ok(Self {
                lib,
                channel,
                is_fd: true,
                start,
                timestamp: HardwareTimebase::new(1e-6, None),
                name: format!(
                    "PCAN_USBBUS{} FD @{}/{}",
                    cfg.channel_index + 1,
                    normalize_baud(&cfg.baud),
                    normalize_baud(&cfg.data_baud)
                ),
            })
        }
    }
}

impl Drop for PcanBus {
    fn drop(&mut self) {
        unsafe {
            if let Ok(uninit) = self.lib.get::<pcan_ffi::FnUninit>(b"CAN_Uninitialize\0") {
                let _ = uninit(self.channel);
            }
        }
    }
}

impl CanAdapter for PcanBus {
    fn poll(&mut self, out: &mut Vec<CanFrame>) -> PollReport {
        use pcan_ffi::*;
        let mut report = PollReport::default();
        unsafe {
            if self.is_fd {
                let read_fd: libloading::Symbol<FnReadFd> = match self.lib.get(b"CAN_ReadFD\0") {
                    Ok(s) => s,
                    Err(error) => {
                        return PollReport {
                            driver_errors: 1,
                            connection_lost: true,
                            message: Some(format!("找不到 CAN_ReadFD: {error}")),
                            ..Default::default()
                        };
                    }
                };
                for _ in 0..512 {
                    let mut msg = TPCANMsgFD {
                        id: 0,
                        msgtype: 0,
                        dlc: 0,
                        data: [0; 64],
                    };
                    let mut ts: u64 = 0;
                    let st = read_fd(self.channel, &mut msg, &mut ts);
                    if st == PCAN_ERROR_QRCVEMPTY {
                        break;
                    }
                    if st != PCAN_ERROR_OK {
                        report = pcan_poll_error(st);
                        break;
                    }
                    if msg.msgtype & MSGTYPE_STATUS != 0 {
                        continue;
                    }
                    let is_fd = msg.msgtype & MSGTYPE_FD != 0;
                    let len = if is_fd {
                        fd_dlc_to_len(msg.dlc)
                    } else {
                        (msg.dlc as usize).min(8)
                    };
                    out.push(CanFrame {
                        t: self.timestamp.map(ts, self.start.elapsed().as_secs_f64()),
                        ch: 1,
                        tx: false,
                        id: msg.id,
                        ext: msg.msgtype & MSGTYPE_EXTENDED != 0,
                        fd: is_fd,
                        brs: msg.msgtype & MSGTYPE_BRS != 0,
                        remote: msg.msgtype & MSGTYPE_RTR != 0,
                        error: msg.msgtype & MSGTYPE_ERRFRAME != 0,
                        data: msg.data[..len.min(64)].to_vec(),
                    });
                }
                return report;
            }
            let read: libloading::Symbol<FnRead> = match self.lib.get(b"CAN_Read\0") {
                Ok(s) => s,
                Err(error) => {
                    return PollReport {
                        driver_errors: 1,
                        connection_lost: true,
                        message: Some(format!("找不到 CAN_Read: {error}")),
                        ..Default::default()
                    };
                }
            };
            for _ in 0..512 {
                let mut msg = TPCANMsg {
                    id: 0,
                    msgtype: 0,
                    len: 0,
                    data: [0; 8],
                };
                let mut ts = TPCANTimestamp {
                    millis: 0,
                    millis_overflow: 0,
                    micros: 0,
                };
                let st = read(self.channel, &mut msg, &mut ts);
                if st == PCAN_ERROR_QRCVEMPTY {
                    break;
                }
                if st != PCAN_ERROR_OK {
                    report = pcan_poll_error(st);
                    break;
                }
                if msg.msgtype & MSGTYPE_STATUS != 0 {
                    continue;
                }
                let len = (msg.len as usize).min(8);
                let timestamp_micros = ((ts.millis_overflow as u64) << 32)
                    .saturating_add(ts.millis as u64)
                    .saturating_mul(1_000)
                    .saturating_add(ts.micros as u64);
                out.push(CanFrame {
                    t: self
                        .timestamp
                        .map(timestamp_micros, self.start.elapsed().as_secs_f64()),
                    ch: 1,
                    tx: false,
                    id: msg.id,
                    ext: msg.msgtype & MSGTYPE_EXTENDED != 0,
                    fd: false,
                    brs: false,
                    remote: msg.msgtype & MSGTYPE_RTR != 0,
                    error: msg.msgtype & MSGTYPE_ERRFRAME != 0,
                    data: msg.data[..len].to_vec(),
                });
            }
        }
        report
    }

    fn send(&mut self, f: &CanFrame) -> Result<(), String> {
        use pcan_ffi::*;
        if self.is_fd {
            unsafe {
                let write: libloading::Symbol<FnWriteFd> = self
                    .lib
                    .get(b"CAN_WriteFD\0")
                    .map_err(|e| format!("找不到 CAN_WriteFD: {e}"))?;
                let len = f.data.len().min(64);
                let mut data = [0u8; 64];
                data[..len].copy_from_slice(&f.data[..len]);
                let send_fd = f.fd || len > 8;
                let mut msgtype = if f.ext {
                    MSGTYPE_EXTENDED
                } else {
                    MSGTYPE_STANDARD
                };
                if send_fd {
                    msgtype |= MSGTYPE_FD;
                    if f.brs {
                        msgtype |= MSGTYPE_BRS;
                    }
                }
                if f.remote {
                    msgtype |= MSGTYPE_RTR;
                }
                let msg = TPCANMsgFD {
                    id: f.id,
                    msgtype,
                    dlc: fd_len_to_dlc(len),
                    data,
                };
                let st = write(self.channel, &msg);
                if st != PCAN_ERROR_OK {
                    return Err(format!("CAN_WriteFD 失败 status=0x{st:08X}"));
                }
            }
            return Ok(());
        }
        if f.fd || f.data.len() > 8 {
            return Err("当前 PCAN 适配器未按 CAN FD 初始化（请在设备配置里选 CAN FD）".into());
        }
        unsafe {
            let write: libloading::Symbol<FnWrite> = self
                .lib
                .get(b"CAN_Write\0")
                .map_err(|e| format!("找不到 CAN_Write: {e}"))?;
            let mut data = [0u8; 8];
            let len = f.data.len().min(8);
            data[..len].copy_from_slice(&f.data[..len]);
            let mut msgtype = if f.ext {
                MSGTYPE_EXTENDED
            } else {
                MSGTYPE_STANDARD
            };
            if f.remote {
                msgtype |= MSGTYPE_RTR;
            }
            let msg = TPCANMsg {
                id: f.id,
                msgtype,
                len: len as u8,
                data,
            };
            let st = write(self.channel, &msg);
            if st != PCAN_ERROR_OK {
                return Err(format!("CAN_Write 失败 status=0x{st:08X}"));
            }
        }
        Ok(())
    }

    fn name(&self) -> &str {
        &self.name
    }
}
