//! zcan responsibilities extracted from src/can.rs.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ZcanDeviceFamily {
    UsbClassic,
    UsbCanFd,
    NetworkTcp,
    NetworkUdp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ZcanDeviceProfile {
    pub(super) device_type: u32,
    pub(super) family: ZcanDeviceFamily,
    pub(super) fd_capable: bool,
}

impl ZcanDeviceProfile {
    pub(super) fn is_network(self) -> bool {
        matches!(
            self.family,
            ZcanDeviceFamily::NetworkTcp | ZcanDeviceFamily::NetworkUdp
        )
    }

    pub(super) fn is_tcp(self) -> bool {
        self.family == ZcanDeviceFamily::NetworkTcp
    }
}

pub(super) fn zcan_driver_channel_type(profile: ZcanDeviceProfile, frame_fd_enabled: bool) -> u8 {
    if profile.family == ZcanDeviceFamily::UsbCanFd || frame_fd_enabled {
        zcan_ffi::TYPE_CANFD
    } else {
        zcan_ffi::TYPE_CAN
    }
}

pub(super) fn load_zlg_library(relative_path: &str) -> Result<libloading::Library, String> {
    let executable =
        std::env::current_exe().map_err(|error| format!("无法定位程序目录: {error}"))?;
    let path = executable
        .parent()
        .ok_or_else(|| "无法定位程序目录".to_string())?
        .join(relative_path);
    unsafe { libloading::Library::new(&path) }
        .map_err(|error| format!("加载 {} 失败: {error}", path.display()))
}

pub(super) fn pinned_zlgcan_library() -> Result<&'static libloading::Library, String> {
    static LIBRARY: std::sync::OnceLock<Result<libloading::Library, String>> =
        std::sync::OnceLock::new();
    match LIBRARY.get_or_init(|| load_zlg_library("zlgcan.dll")) {
        Ok(library) => Ok(library),
        Err(error) => Err(error.clone()),
    }
}

pub(super) fn pin_zlg_kernel_library(profile: ZcanDeviceProfile) -> Result<(), String> {
    static USB_CLASSIC_LEGACY: std::sync::OnceLock<Result<libloading::Library, String>> =
        std::sync::OnceLock::new();
    static USB_CLASSIC: std::sync::OnceLock<Result<libloading::Library, String>> =
        std::sync::OnceLock::new();
    static USB_CAN_FD: std::sync::OnceLock<Result<libloading::Library, String>> =
        std::sync::OnceLock::new();
    static USB_CAN_FD_800: std::sync::OnceLock<Result<libloading::Library, String>> =
        std::sync::OnceLock::new();
    let pinned = match profile.family {
        ZcanDeviceFamily::UsbClassic if matches!(profile.device_type, 3 | 4) => {
            USB_CLASSIC_LEGACY.get_or_init(|| load_zlg_library("kerneldlls/USBCAN.dll"))
        }
        ZcanDeviceFamily::UsbClassic => {
            USB_CLASSIC.get_or_init(|| load_zlg_library("kerneldlls/USBCAN_E_64.dll"))
        }
        ZcanDeviceFamily::UsbCanFd if profile.device_type == 59 => {
            USB_CAN_FD_800.get_or_init(|| load_zlg_library("kerneldlls/USBCANFD800U.dll"))
        }
        ZcanDeviceFamily::UsbCanFd => {
            USB_CAN_FD.get_or_init(|| load_zlg_library("kerneldlls/USBCANFD.dll"))
        }
        ZcanDeviceFamily::NetworkTcp | ZcanDeviceFamily::NetworkUdp => return Ok(()),
    };
    pinned.as_ref().map(|_| ()).map_err(Clone::clone)
}

pub(super) fn zcan_profile(device_type: &str) -> Option<ZcanDeviceProfile> {
    match device_type
        .trim()
        .to_ascii_uppercase()
        .replace(' ', "")
        .as_str()
    {
        "USBCAN1" => Some(ZcanDeviceProfile {
            device_type: 3,
            family: ZcanDeviceFamily::UsbClassic,
            fd_capable: false,
        }),
        "USBCAN" | "USBCAN2" => Some(ZcanDeviceProfile {
            device_type: 4,
            family: ZcanDeviceFamily::UsbClassic,
            fd_capable: false,
        }),
        "USBCANFD" | "USBCANFD-200U" | "USBCANFD200U" => Some(ZcanDeviceProfile {
            device_type: 41,
            family: ZcanDeviceFamily::UsbCanFd,
            fd_capable: true,
        }),
        "USBCANFD-100U" | "USBCANFD100U" => Some(ZcanDeviceProfile {
            device_type: 42,
            family: ZcanDeviceFamily::UsbCanFd,
            fd_capable: true,
        }),
        "USBCANFD-MINI" | "USBCANFDMINI" => Some(ZcanDeviceProfile {
            device_type: 43,
            family: ZcanDeviceFamily::UsbCanFd,
            fd_capable: true,
        }),
        "USBCANFD-800U" | "USBCANFD800U" => Some(ZcanDeviceProfile {
            device_type: 59,
            family: ZcanDeviceFamily::UsbCanFd,
            fd_capable: true,
        }),
        "USBCAN-E-U" | "USBCANEU" => Some(ZcanDeviceProfile {
            device_type: 20,
            family: ZcanDeviceFamily::UsbClassic,
            fd_capable: false,
        }),
        "USBCAN-2E-U" | "USBCAN2EU" => Some(ZcanDeviceProfile {
            device_type: 21,
            family: ZcanDeviceFamily::UsbClassic,
            fd_capable: false,
        }),
        "CANFDNET" | "CANFDNET-TCP" | "CANFDNETTCP" => Some(ZcanDeviceProfile {
            device_type: 48,
            family: ZcanDeviceFamily::NetworkTcp,
            fd_capable: true,
        }),
        "CANFDNET-UDP" | "CANFDNETUDP" => Some(ZcanDeviceProfile {
            device_type: 49,
            family: ZcanDeviceFamily::NetworkUdp,
            fd_capable: true,
        }),
        "CANFDWIFI" | "CANFDWIFI-TCP" | "CANFDWIFITCP" => Some(ZcanDeviceProfile {
            device_type: 50,
            family: ZcanDeviceFamily::NetworkTcp,
            fd_capable: true,
        }),
        "CANFDWIFI-UDP" | "CANFDWIFIUDP" => Some(ZcanDeviceProfile {
            device_type: 51,
            family: ZcanDeviceFamily::NetworkUdp,
            fd_capable: true,
        }),
        _ => None,
    }
}

pub(super) fn zcan_info_text(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

pub fn zcan_attached_channels() -> Vec<ZcanUsbChannelInfo> {
    use zcan_ffi::*;

    let mut detected = Vec::new();
    // Device types in each family are aliases in the current ZLG driver. Open
    // one canonical type, then use ZCAN_GetDeviceInf to identify the actual
    // model and physical channel count.
    for requested_type in ["USBCANFD-200U", "USBCAN-E-U", "USBCAN1", "USBCAN2"] {
        let Some(profile) = zcan_profile(requested_type) else {
            continue;
        };
        if pin_zlg_kernel_library(profile).is_err() {
            continue;
        }
        let Ok(lib) = pinned_zlgcan_library() else {
            continue;
        };
        unsafe {
            let Ok(open) = lib.get::<FnOpenDevice>(b"ZCAN_OpenDevice\0") else {
                continue;
            };
            let Ok(close) = lib.get::<FnCloseDevice>(b"ZCAN_CloseDevice\0") else {
                continue;
            };
            let Ok(get_info) = lib.get::<FnGetDeviceInfo>(b"ZCAN_GetDeviceInf\0") else {
                continue;
            };
            for device_index in 0..8 {
                let handle = open(profile.device_type, device_index, 0);
                if handle.is_null() {
                    if device_index == 0 {
                        continue;
                    }
                    break;
                }
                let mut info = ZcanDeviceInfo {
                    hw_version: 0,
                    fw_version: 0,
                    driver_version: 0,
                    interface_version: 0,
                    irq_num: 0,
                    can_num: 0,
                    serial_number: [0; 20],
                    hardware_type: [0; 40],
                    reserved: [0; 4],
                };
                let info_ok = get_info(handle, &mut info) == 1;
                let _ = close(handle);
                if !info_ok {
                    continue;
                }
                let raw_hardware = zcan_info_text(&info.hardware_type);
                let upper = raw_hardware.to_ascii_uppercase();
                let fd_capable = profile.family == ZcanDeviceFamily::UsbCanFd;
                let device_type = if profile.device_type == 3 {
                    "USBCAN1"
                } else if profile.device_type == 4 {
                    "USBCAN2"
                } else if fd_capable {
                    if upper.contains("200U") {
                        "USBCANFD-200U"
                    } else if upper.contains("100U") {
                        "USBCANFD-100U"
                    } else if upper.contains("MINI") {
                        "USBCANFD-MINI"
                    } else if upper.contains("800U") {
                        "USBCANFD-800U"
                    } else {
                        "USBCANFD-200U"
                    }
                } else if info.can_num > 1 {
                    "USBCAN-2E-U"
                } else {
                    "USBCAN-E-U"
                };
                let serial_number = zcan_info_text(&info.serial_number);
                let channel_count = u32::from(info.can_num.max(1));
                for channel_index in 0..channel_count {
                    detected.push(ZcanUsbChannelInfo {
                        device_type: device_type.to_string(),
                        hardware_label: if raw_hardware.is_empty() {
                            device_type.to_string()
                        } else {
                            raw_hardware.clone()
                        },
                        serial_number: serial_number.clone(),
                        device_index,
                        channel_index,
                        fd_capable,
                    });
                }
            }
        }
    }
    // Some driver generations accept both legacy USBCAN1 and USBCAN2 type
    // codes for the same box. Keep one physical endpoint when the board serial
    // and channel identity coincide.
    let mut seen_zlg = std::collections::HashSet::new();
    detected.retain(|channel| {
        let identity = if channel.serial_number.is_empty() {
            format!(
                "{}:{}:{}",
                channel.device_type, channel.device_index, channel.channel_index
            )
        } else {
            format!("{}:{}", channel.serial_number, channel.channel_index)
        };
        seen_zlg.insert(identity)
    });
    detected.extend(legacy_vci_attached_channels());
    detected
}

pub(super) fn legacy_vci_attached_channels() -> Vec<ZcanUsbChannelInfo> {
    let mut detected = Vec::new();
    detected.extend(probe_vci_devices(
        &["ECanVci64.dll", "ECanVci.dll"],
        "",
        "GCAN",
        "GCAN USBCAN-I",
        3,
        1,
    ));
    detected.extend(probe_vci_devices(
        &["ControlCAN.dll"],
        "VCI_",
        "ZHCX",
        "CANalyst-II",
        4,
        0,
    ));
    detected
}

pub(super) fn probe_vci_devices(
    dll_candidates: &[&str],
    prefix: &'static str,
    device_name: &str,
    fallback_label: &str,
    device_type: u32,
    fixed_channel_count: u8,
) -> Vec<ZcanUsbChannelInfo> {
    use zlg_ffi::*;

    unsafe {
        let mut detected = Vec::new();
        let mut consecutive_misses = 0;
        for device_index in 0..8 {
            let Ok((key, device)) =
                get_or_open_vci_device(dll_candidates, prefix, device_type, device_index)
            else {
                consecutive_misses += 1;
                if consecutive_misses >= 2 {
                    break;
                }
                continue;
            };
            consecutive_misses = 0;
            let Ok(read_board_info) = device
                .lib
                .get::<FnReadBoardInfo>(device.sym("ReadBoardInfo").as_slice())
            else {
                continue;
            };
            let mut info = VCI_BOARD_INFO::default();
            let info_ok = read_board_info(device_type, device_index, &mut info) == 1;
            if !info_ok {
                drop(read_board_info);
                evict_vci_device(&key, &device);
                continue;
            }

            let serial_number = printable_vci_text(&info.serial_number);
            let reported_label = printable_vci_text(&info.hardware_type);
            let hardware_label = if device_name == "GCAN" || reported_label.is_empty() {
                fallback_label.to_string()
            } else {
                reported_label
            };
            let channel_count = if fixed_channel_count == 0 {
                info.can_num.clamp(1, 8)
            } else {
                fixed_channel_count
            };
            for channel_index in 0..u32::from(channel_count) {
                detected.push(ZcanUsbChannelInfo {
                    device_type: device_name.to_string(),
                    hardware_label: hardware_label.clone(),
                    serial_number: serial_number.clone(),
                    device_index,
                    channel_index,
                    fd_capable: false,
                });
            }
        }
        detected
    }
}

pub(super) fn printable_vci_text(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    bytes[..end]
        .iter()
        .copied()
        .filter(|byte| byte.is_ascii_graphic() || *byte == b' ')
        .map(char::from)
        .collect::<String>()
        .trim()
        .to_string()
}

pub(super) fn baud_to_bps(s: &str) -> u32 {
    let b = s
        .trim()
        .to_ascii_uppercase()
        .replace(' ', "")
        .replace("BPS", "");
    if let Some(x) = b.strip_suffix('M') {
        return (x.parse::<f64>().unwrap_or(0.0) * 1_000_000.0) as u32;
    }
    if let Some(x) = b.strip_suffix('K') {
        return (x.parse::<f64>().unwrap_or(0.0) * 1_000.0) as u32;
    }
    b.parse::<u32>().unwrap_or(0)
}

pub(super) fn adapter_key(cfg: &DeviceConfig) -> String {
    if !cfg.hardware_id.trim().is_empty() {
        return cfg.hardware_id.trim().to_ascii_uppercase();
    }
    let device = cfg
        .device_type
        .trim()
        .to_ascii_uppercase()
        .replace([' ', '_'], "");
    if zcan_profile(&cfg.device_type).is_some_and(ZcanDeviceProfile::is_network) {
        format!(
            "{device}:{}:{}:{}",
            cfg.ip.trim(),
            cfg.port.trim(),
            cfg.channel_index
        )
    } else {
        format!("{device}:{}:{}", cfg.device_index, cfg.channel_index)
    }
}

pub fn validate_device_config(cfg: &DeviceConfig) -> Result<(), String> {
    if cfg.sw_channel == 0 {
        return Err("软件 CAN 通道必须从 1 开始".into());
    }
    let device = cfg.device_type.trim().to_ascii_uppercase();
    if device.is_empty() {
        return Err(format!("CAN{} 未选择设备类型", cfg.sw_channel));
    }
    if matches!(device.as_str(), "VIRTUAL" | "SIM") {
        return Err(format!(
            "CAN{} 的虚拟总线已移除，请选择硬件适配器",
            cfg.sw_channel
        ));
    }

    if device == "PCAN" {
        if cfg.channel_index >= 16 {
            return Err(format!("CAN{} PCAN 硬件通道必须为 0..15", cfg.sw_channel));
        }
        if cfg.is_fd {
            if cfg.custom_bitrate.trim().is_empty() {
                pcan_fd_bitrate(&cfg.baud, &cfg.data_baud)?;
            } else {
                pcan_fd_bitrate(cfg.custom_bitrate.trim(), &cfg.data_baud)?;
            }
        } else if !matches!(
            normalize_baud(&cfg.baud).as_str(),
            "125K" | "250K" | "500K" | "1000K"
        ) {
            return Err(format!(
                "CAN{} PCAN 不支持波特率 {}",
                cfg.sw_channel, cfg.baud
            ));
        }
        if cfg.listen_only {
            return Err(format!(
                "CAN{} PCAN 监听模式尚未由当前后端实现",
                cfg.sw_channel
            ));
        }
        if cfg.fd_non_iso {
            return Err(format!(
                "CAN{} PCAN Non-ISO CAN FD 尚未由当前后端实现",
                cfg.sw_channel
            ));
        }
        return Ok(());
    }

    if matches!(device.as_str(), "GCAN" | "ZHCX" | "ZHCXCAN") {
        if !cfg.custom_bitrate.trim().is_empty() {
            return Err(format!(
                "CAN{} 的 {} 不支持 PCAN 自定义位时序串",
                cfg.sw_channel, cfg.device_type
            ));
        }
        if cfg.is_fd {
            return Err(format!(
                "CAN{} 的 {} 仅支持 Classical CAN",
                cfg.sw_channel, cfg.device_type
            ));
        }
        if cfg.fd_non_iso {
            return Err(format!(
                "CAN{} 的 {} 不支持 Non-ISO CAN FD",
                cfg.sw_channel, cfg.device_type
            ));
        }
        if zlg_timing(&cfg.baud).is_none() {
            return Err(format!(
                "CAN{} 的 {} 不支持波特率 {}",
                cfg.sw_channel, cfg.device_type, cfg.baud
            ));
        }
        return Ok(());
    }

    if let Some(profile) = zcan_profile(&cfg.device_type) {
        if !cfg.custom_bitrate.trim().is_empty() {
            return Err(format!(
                "CAN{} 的 {} 不支持 PCAN 自定义位时序串",
                cfg.sw_channel, cfg.device_type
            ));
        }
        if cfg.is_fd && !profile.fd_capable {
            return Err(format!(
                "CAN{} 的 {} 不支持 CAN FD",
                cfg.sw_channel, cfg.device_type
            ));
        }
        if cfg.fd_non_iso && (!cfg.is_fd || !profile.fd_capable) {
            return Err(format!(
                "CAN{} 只有 CAN FD 硬件才能使用 Non-ISO 模式",
                cfg.sw_channel
            ));
        }
        let arbitration = baud_to_bps(&cfg.baud);
        if arbitration == 0 {
            return Err(format!(
                "CAN{} 仲裁波特率无效: {}",
                cfg.sw_channel, cfg.baud
            ));
        }
        if cfg.is_fd {
            let data = baud_to_bps(&cfg.data_baud);
            if data == 0 {
                return Err(format!(
                    "CAN{} 数据波特率无效: {}",
                    cfg.sw_channel, cfg.data_baud
                ));
            }
            if data < arbitration {
                return Err(format!(
                    "CAN{} 数据波特率 {} 不能低于仲裁波特率 {}",
                    cfg.sw_channel, cfg.data_baud, cfg.baud
                ));
            }
        }
        if profile.is_network() {
            cfg.ip
                .trim()
                .parse::<std::net::IpAddr>()
                .map_err(|_| format!("CAN{} 网络适配器 IP 无效: {}", cfg.sw_channel, cfg.ip))?;
            let port =
                cfg.port.trim().parse::<u16>().map_err(|_| {
                    format!("CAN{} 网络适配器端口无效: {}", cfg.sw_channel, cfg.port)
                })?;
            if port == 0 {
                return Err(format!("CAN{} 网络适配器端口不能为 0", cfg.sw_channel));
            }
        }
        return Ok(());
    }

    Err(format!(
        "CAN{} 未知设备类型: {}",
        cfg.sw_channel, cfg.device_type
    ))
}

pub fn validate_channel_set(cfgs: &[DeviceConfig]) -> Result<(), String> {
    if cfgs.is_empty() {
        return Err("至少需要配置一个 CAN 通道".into());
    }
    let mut software_channels = std::collections::HashSet::new();
    let mut adapters = std::collections::HashSet::new();
    for cfg in cfgs {
        validate_device_config(cfg)?;
        if !software_channels.insert(cfg.sw_channel) {
            return Err(format!("软件通道 CAN{} 重复", cfg.sw_channel));
        }
        let key = adapter_key(cfg);
        if !adapters.insert(key) {
            return Err(format!(
                "CAN{} 与其他通道绑定了同一硬件端点 {} dev{} ch{}",
                cfg.sw_channel, cfg.device_type, cfg.device_index, cfg.channel_index
            ));
        }
    }
    Ok(())
}

#[allow(non_camel_case_types)]
pub(super) mod zcan_ffi {
    use std::os::raw::{c_char, c_void};
    pub type DevHandle = *mut c_void;
    pub type ChHandle = *mut c_void;

    pub type FnOpenDevice = unsafe extern "system" fn(u32, u32, u32) -> DevHandle;
    pub type FnCloseDevice = unsafe extern "system" fn(DevHandle) -> u32;
    pub type FnGetDeviceInfo = unsafe extern "system" fn(DevHandle, *mut ZcanDeviceInfo) -> u32;
    pub type FnIsDeviceOnline = unsafe extern "system" fn(DevHandle) -> u32;
    pub type FnInitCan =
        unsafe extern "system" fn(DevHandle, u32, *mut ZcanChannelInitConfig) -> ChHandle;
    pub type FnStartCan = unsafe extern "system" fn(ChHandle) -> u32;
    pub type FnResetCan = unsafe extern "system" fn(ChHandle) -> u32;
    pub type FnClearBuffer = unsafe extern "system" fn(ChHandle) -> u32;
    pub type FnReadChannelErrInfo =
        unsafe extern "system" fn(ChHandle, *mut ZcanChannelErrInfo) -> u32;
    pub type FnReadChannelStatus =
        unsafe extern "system" fn(ChHandle, *mut ZcanChannelStatus) -> u32;
    pub type FnSetValue = unsafe extern "system" fn(DevHandle, *const c_char, *const c_char) -> u32;
    pub type FnGetValue = unsafe extern "system" fn(DevHandle, *const c_char) -> *const c_void;
    pub type FnGetReceiveNum = unsafe extern "system" fn(ChHandle, u8) -> u32;
    pub type FnTransmit = unsafe extern "system" fn(ChHandle, *const ZcanTransmitData, u32) -> u32;
    pub type FnTransmitFd =
        unsafe extern "system" fn(ChHandle, *const ZcanTransmitFdData, u32) -> u32;
    pub type FnReceive = unsafe extern "system" fn(ChHandle, *mut ZcanReceiveData, u32, i32) -> u32;
    pub type FnReceiveFd =
        unsafe extern "system" fn(ChHandle, *mut ZcanReceiveFdData, u32, i32) -> u32;

    pub const EFF: u32 = 0x8000_0000;
    pub const RTR: u32 = 0x4000_0000;
    pub const ID_MASK: u32 = 0x1FFF_FFFF;
    pub const TYPE_CAN: u8 = 0;
    pub const TYPE_CANFD: u8 = 1;

    pub const ERROR_CAN_OVERFLOW: u32 = 0x0001;
    pub const ERROR_CAN_ERRALARM: u32 = 0x0002;
    pub const ERROR_CAN_PASSIVE: u32 = 0x0004;
    pub const ERROR_CAN_LOSE: u32 = 0x0008;
    pub const ERROR_CAN_BUSERR: u32 = 0x0010;
    pub const ERROR_CAN_BUSOFF: u32 = 0x0020;
    pub const ERROR_CAN_BUFFER_OVERFLOW: u32 = 0x0040;
    pub const ERROR_DEVICEOPENED: u32 = 0x0100;
    pub const ERROR_DEVICEOPEN: u32 = 0x0200;
    pub const ERROR_DEVICENOTOPEN: u32 = 0x0400;
    pub const ERROR_BUFFEROVERFLOW: u32 = 0x0800;
    pub const ERROR_DEVICENOTEXIST: u32 = 0x1000;
    pub const ERROR_LOADKERNELDLL: u32 = 0x2000;
    pub const ERROR_CMDFAILED: u32 = 0x4000;
    pub const ERROR_BUFFERCREATE: u32 = 0x8000;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct ZcanDeviceInfo {
        pub hw_version: u16,
        pub fw_version: u16,
        pub driver_version: u16,
        pub interface_version: u16,
        pub irq_num: u16,
        pub can_num: u8,
        pub serial_number: [u8; 20],
        pub hardware_type: [u8; 40],
        pub reserved: [u16; 4],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CanFrameC {
        pub can_id: u32,
        pub can_dlc: u8,
        pub pad: u8,
        pub res0: u8,
        pub res1: u8,
        pub data: [u8; 8],
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CanfdFrameC {
        pub can_id: u32,
        pub len: u8,
        pub flags: u8,
        pub res0: u8,
        pub res1: u8,
        pub data: [u8; 64],
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct ZcanTransmitData {
        pub frame: CanFrameC,
        pub transmit_type: u32,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct ZcanTransmitFdData {
        pub frame: CanfdFrameC,
        pub transmit_type: u32,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct ZcanReceiveData {
        pub frame: CanFrameC,
        pub timestamp: u64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct ZcanReceiveFdData {
        pub frame: CanfdFrameC,
        pub timestamp: u64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct ZcanClassicInitConfig {
        pub acc_code: u32,
        pub acc_mask: u32,
        pub reserved: u32,
        pub filter: u8,
        pub timing0: u8,
        pub timing1: u8,
        pub mode: u8,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub union ZcanChannelConfig {
        pub classic: ZcanClassicInitConfig,
        pub raw: [u8; 28],
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct ZcanChannelInitConfig {
        pub can_type: u32,
        pub config: ZcanChannelConfig,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct ZcanChannelErrInfo {
        pub error_code: u32,
        pub passive_err_data: [u8; 3],
        pub ar_lost_err_data: u8,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct ZcanChannelStatus {
        pub err_interrupt: u8,
        pub reg_mode: u8,
        pub reg_status: u8,
        pub reg_al_capture: u8,
        pub reg_ec_capture: u8,
        pub reg_ew_limit: u8,
        pub reg_re_counter: u8,
        pub reg_te_counter: u8,
        pub reserved: u32,
    }
}

pub(super) fn zcan_error_is_connection_lost(error_code: u32) -> bool {
    use zcan_ffi::*;
    error_code
        & (ERROR_DEVICEOPEN | ERROR_DEVICENOTOPEN | ERROR_DEVICENOTEXIST | ERROR_LOADKERNELDLL)
        != 0
}

pub(super) fn zcan_error_message(
    error_code: u32,
    status: Option<zcan_ffi::ZcanChannelStatus>,
) -> String {
    use zcan_ffi::*;
    let mut causes = Vec::new();
    if error_code & ERROR_CAN_OVERFLOW != 0 {
        causes.push("控制器接收溢出");
    }
    if error_code & ERROR_CAN_BUFFER_OVERFLOW != 0 || error_code & ERROR_BUFFEROVERFLOW != 0 {
        causes.push("驱动接收缓冲区溢出");
    }
    if error_code & ERROR_CAN_ERRALARM != 0 {
        causes.push("错误计数达到报警阈值");
    }
    if error_code & ERROR_CAN_PASSIVE != 0 {
        causes.push("CAN 控制器进入错误被动状态");
    }
    if error_code & ERROR_CAN_LOSE != 0 {
        causes.push("仲裁丢失");
    }
    if error_code & ERROR_CAN_BUSERR != 0 {
        causes
            .push("CAN 总线错误：检查 CAN_H/CAN_L、共地、两端 120Ω、波特率以及是否存在可应答节点");
    }
    if error_code & ERROR_CAN_BUSOFF != 0 {
        causes.push("CAN 控制器 Bus-Off");
    }
    if error_code & ERROR_DEVICEOPENED != 0 {
        causes.push("设备已被其他程序占用");
    }
    if error_code & ERROR_DEVICEOPEN != 0 {
        causes.push("设备打开失败");
    }
    if error_code & ERROR_DEVICENOTOPEN != 0 {
        causes.push("设备未打开");
    }
    if error_code & ERROR_DEVICENOTEXIST != 0 {
        causes.push("设备不存在或 USB 已断开");
    }
    if error_code & ERROR_LOADKERNELDLL != 0 {
        causes.push("ZLG 内核驱动 DLL 加载失败");
    }
    if error_code & ERROR_CMDFAILED != 0 {
        causes.push("驱动命令执行失败");
    }
    if error_code & ERROR_BUFFERCREATE != 0 {
        causes.push("驱动缓冲区创建失败");
    }
    if causes.is_empty() {
        causes.push("未知 ZLG 驱动错误");
    }
    let counters = status
        .map(|s| {
            format!(
                "，RXErr={} TXErr={} Status=0x{:02X}",
                s.reg_re_counter, s.reg_te_counter, s.reg_status
            )
        })
        .unwrap_or_default();
    format!(
        "ZLG 错误 0x{error_code:08X}：{}{counters}",
        causes.join("；")
    )
}

pub(super) fn merge_poll_report(target: &mut PollReport, source: PollReport) {
    target.receive_overruns = target
        .receive_overruns
        .saturating_add(source.receive_overruns);
    target.driver_errors = target.driver_errors.saturating_add(source.driver_errors);
    target.connection_lost |= source.connection_lost;
    if source.message.is_some() {
        target.message = source.message;
    }
}

pub(super) struct ZcanSharedDevice {
    pub(super) lib: &'static libloading::Library,
    pub(super) dev: usize,
}

impl Drop for ZcanSharedDevice {
    fn drop(&mut self) {
        unsafe {
            if let Ok(close) = self
                .lib
                .get::<zcan_ffi::FnCloseDevice>(b"ZCAN_CloseDevice\0")
            {
                let _ = close(self.dev as zcan_ffi::DevHandle);
            }
        }
    }
}

pub(super) fn acquire_zcan_device(
    profile: ZcanDeviceProfile,
    cfg: &DeviceConfig,
) -> Result<Arc<ZcanSharedDevice>, String> {
    use zcan_ffi::*;
    static DEVICES: std::sync::OnceLock<Mutex<HashMap<String, Weak<ZcanSharedDevice>>>> =
        std::sync::OnceLock::new();
    let key = zcan_device_key(profile, cfg);
    let mut devices = DEVICES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| "ZLG 设备共享状态已损坏".to_string())?;
    if let Some(device) = devices.get(&key).and_then(Weak::upgrade) {
        return Ok(device);
    }

    pin_zlg_kernel_library(profile)?;
    let lib = pinned_zlgcan_library()?;
    let dev = unsafe {
        let open: libloading::Symbol<FnOpenDevice> = lib
            .get(b"ZCAN_OpenDevice\0")
            .map_err(|error| format!("ZCAN_OpenDevice 未找到: {error}"))?;
        open(profile.device_type, cfg.device_index, 0)
    };
    if dev.is_null() {
        return Err(if profile.family == ZcanDeviceFamily::UsbClassic {
            "ZCAN_OpenDevice 失败（USBCAN-E-U 驱动未启动或设备被占用；请重新插拔设备，或以管理员权限重新安装官方驱动）".into()
        } else {
            "ZCAN_OpenDevice 失败（设备未连接、被其他程序占用或 ZLG 驱动不可用；请关闭 ZCANPRO 等工具、重新插拔设备后重试）".into()
        });
    }
    let device = Arc::new(ZcanSharedDevice {
        lib,
        dev: dev as usize,
    });
    devices.insert(key, Arc::downgrade(&device));
    Ok(device)
}

pub(super) fn zcan_device_key(profile: ZcanDeviceProfile, cfg: &DeviceConfig) -> String {
    if profile.is_network() {
        format!(
            "net:{}:{}:{}:{}:{}",
            profile.device_type, cfg.device_index, cfg.ip, cfg.port, cfg.net_server
        )
    } else {
        // USB rows can retain hidden network-form values after the user changes
        // device type. Those values must never split one physical multi-channel
        // adapter into multiple ZCAN_OpenDevice calls.
        format!("usb:{}:{}", profile.device_type, cfg.device_index)
    }
}

pub struct ZcanFdBus {
    pub(super) device: Arc<ZcanSharedDevice>,
    pub(super) ch: usize,
    pub(super) channel_is_fd: bool,
    pub(super) listen_only: bool,
    pub(super) start: Instant,
    pub(super) timestamp: HardwareTimebase,
    pub(super) name: String,
    pub(super) last_health_check: Instant,
    pub(super) last_error_code: u32,
    pub(super) last_busoff_recovery: Option<Instant>,
    pub(super) last_error_counters: Option<(u8, u8)>,
    pub(super) pending_error_frames: Vec<CanFrame>,
}

impl ZcanFdBus {
    pub fn open(start: Instant, cfg: &DeviceConfig) -> Result<Self, String> {
        use std::ffi::CString;
        use zcan_ffi::*;
        let profile = zcan_profile(&cfg.device_type)
            .ok_or_else(|| format!("非新版 ZLG 设备类型: {}", cfg.device_type))?;
        let is_net = profile.is_network();
        let is_usbcanfd = profile.family == ZcanDeviceFamily::UsbCanFd;
        let is_usbcan_e_u = profile.family == ZcanDeviceFamily::UsbClassic;
        unsafe {
            let device = acquire_zcan_device(profile, cfg)?;
            let lib = device.lib;
            let dev = device.dev as DevHandle;

            let setval: libloading::Symbol<FnSetValue> = match lib.get(b"ZCAN_SetValue\0") {
                Ok(s) => s,
                Err(e) => {
                    return Err(format!("ZCAN_SetValue 未找到: {e}"));
                }
            };
            let set = |key: &str, val: &str| -> Result<(), String> {
                let path = CString::new(format!("{}/{}", cfg.channel_index, key)).unwrap();
                let v = CString::new(val).unwrap();
                if setval(dev, path.as_ptr(), v.as_ptr()) != 1 {
                    Err(format!("设置 {key}={val} 失败"))
                } else {
                    Ok(())
                }
            };
            let cfg_res: Result<(), String> = if is_net {
                let mode = if profile.is_tcp() {
                    set("work_mode", if cfg.net_server { "1" } else { "0" })
                } else {
                    set("local_port", &cfg.port)
                };
                mode.and_then(|_| set("ip", &cfg.ip))
                    .and_then(|_| set("work_port", &cfg.port))
            } else if is_usbcanfd {
                let _ = set("canfd_standard", if cfg.fd_non_iso { "1" } else { "0" });
                let _ = set("work_mode", if cfg.listen_only { "1" } else { "0" });
                let abit = baud_to_bps(&cfg.baud).to_string();
                let dbit = if cfg.is_fd {
                    baud_to_bps(&cfg.data_baud).to_string()
                } else {
                    abit.clone()
                };
                set("canfd_abit_baud_rate", &abit).and_then(|_| set("canfd_dbit_baud_rate", &dbit))
            } else if is_usbcan_e_u {
                // The official USBCAN-E-U device property declares
                // channel_N/baud_rate as an at_initcan="pre" setting.  The
                // classic timing bytes below are still populated for API
                // compatibility, but the property is what the kernel driver
                // uses to select the requested bitrate.
                set("baud_rate", &baud_to_bps(&cfg.baud).to_string())
            } else {
                set("baud_rate", &baud_to_bps(&cfg.baud).to_string())
            };
            cfg_res?;
            let bitrate_readback = if is_usbcan_e_u {
                let path = CString::new(format!("{}/baud_rate", cfg.channel_index)).unwrap();
                lib.get::<FnGetValue>(b"ZCAN_GetValue\0")
                    .ok()
                    .and_then(|get| {
                        let value = get(dev, path.as_ptr()).cast::<std::os::raw::c_char>();
                        (!value.is_null()).then(|| {
                            std::ffi::CStr::from_ptr(value)
                                .to_string_lossy()
                                .into_owned()
                        })
                    })
            } else {
                None
            };
            drop(setval);

            let init: libloading::Symbol<FnInitCan> = match lib.get(b"ZCAN_InitCAN\0") {
                Ok(s) => s,
                Err(e) => {
                    return Err(format!("ZCAN_InitCAN 未找到: {e}"));
                }
            };
            // The official driver properties above are the single source of
            // bitrate configuration. In particular, USBCAN-E-U declares
            // `baud_rate` as an at_initcan="pre" property. Mixing that with
            // legacy VCI Timing0/Timing1 bytes can produce a different actual
            // bitrate on some E-U/MINI sales variants.
            let mut init_cfg = ZcanChannelInitConfig {
                // ZLG's USBCANFD kernel driver only starts these physical
                // channels as TYPE_CANFD. A TYPE_CANFD channel still carries
                // ordinary CAN 2.0 frames; cfg.is_fd controls frame formats.
                can_type: zcan_driver_channel_type(profile, cfg.is_fd) as u32,
                config: ZcanChannelConfig { raw: [0u8; 28] },
            };
            let ch = init(dev, cfg.channel_index, &mut init_cfg);
            drop(init);
            if ch.is_null() {
                return Err("ZCAN_InitCAN 失败".into());
            }

            // A process restart does not necessarily power-cycle the USB CAN
            // controller. Clear a Bus-Off/error-passive state left by an
            // earlier bitrate mismatch before starting the newly configured
            // channel.
            if let Ok(reset) = lib.get::<FnResetCan>(b"ZCAN_ResetCAN\0") {
                let _ = reset(ch);
            }
            if let Ok(clear) = lib.get::<FnClearBuffer>(b"ZCAN_ClearBuffer\0") {
                let _ = clear(ch);
            }

            if is_usbcanfd
                && !is_net
                && let Ok(setres) = lib.get::<FnSetValue>(b"ZCAN_SetValue\0")
            {
                let path =
                    CString::new(format!("{}/initenal_resistance", cfg.channel_index)).unwrap();
                let v = CString::new(if cfg.termination { "1" } else { "0" }).unwrap();
                let _ = setres(dev, path.as_ptr(), v.as_ptr());
            }

            let start_can: libloading::Symbol<FnStartCan> = match lib.get(b"ZCAN_StartCAN\0") {
                Ok(s) => s,
                Err(e) => {
                    return Err(format!("ZCAN_StartCAN 未找到: {e}"));
                }
            };
            if start_can(ch) != 1 {
                drop(start_can);
                if let Ok(reset) = lib.get::<FnResetCan>(b"ZCAN_ResetCAN\0") {
                    let _ = reset(ch);
                }
                return Err("ZCAN_StartCAN 失败".into());
            }
            drop(start_can);

            if let Ok(clear) = lib.get::<FnClearBuffer>(b"ZCAN_ClearBuffer\0") {
                let _ = clear(ch);
            }
            if let Ok(online) = lib.get::<FnIsDeviceOnline>(b"ZCAN_IsDeviceOnLine\0")
                && online(dev) == 0
            {
                return Err("ZLG 设备打开后报告离线，请检查 USB、驱动和设备占用状态".into());
            }

            let name = if is_net {
                format!("{} {}:{}", cfg.device_type, cfg.ip, cfg.port)
            } else {
                format!(
                    "{} dev{} CAN{} @{}{}{}",
                    cfg.device_type,
                    cfg.device_index,
                    cfg.channel_index,
                    normalize_baud(&cfg.baud),
                    if cfg.is_fd {
                        format!("/{}", normalize_baud(&cfg.data_baud))
                    } else {
                        String::new()
                    },
                    bitrate_readback
                        .filter(|value| !value.is_empty())
                        .map(|value| format!(" [driver={value}]"))
                        .unwrap_or_default()
                )
            };
            Ok(Self {
                device,
                ch: ch as usize,
                channel_is_fd: cfg.is_fd,
                listen_only: cfg.listen_only,
                start,
                // The vendor zlgcan.h supplied with the driver declares timestamps in us.
                timestamp: HardwareTimebase::new(1e-6, None),
                name,
                last_health_check: Instant::now() - Duration::from_secs(1),
                last_error_code: 0,
                last_busoff_recovery: None,
                last_error_counters: None,
                pending_error_frames: Vec::new(),
            })
        }
    }

    pub(super) fn health_report(&mut self, force: bool) -> PollReport {
        use zcan_ffi::*;
        if !force && self.last_health_check.elapsed() < Duration::from_millis(200) {
            return PollReport::default();
        }
        self.last_health_check = Instant::now();
        let dev = self.device.dev as DevHandle;
        let ch = self.ch as ChHandle;
        unsafe {
            match self
                .device
                .lib
                .get::<FnIsDeviceOnline>(b"ZCAN_IsDeviceOnLine\0")
            {
                Ok(online) if online(dev) == 0 => {
                    return PollReport {
                        driver_errors: 1,
                        connection_lost: true,
                        message: Some("ZLG 设备离线或 USB 已断开".into()),
                        ..Default::default()
                    };
                }
                Err(error) => {
                    return PollReport {
                        driver_errors: 1,
                        connection_lost: true,
                        message: Some(format!("ZCAN_IsDeviceOnLine 未找到: {error}")),
                        ..Default::default()
                    };
                }
                _ => {}
            }

            let mut error_info = ZcanChannelErrInfo::default();
            let error_code = self
                .device
                .lib
                .get::<FnReadChannelErrInfo>(b"ZCAN_ReadChannelErrInfo\0")
                .ok()
                .filter(|read| read(ch, &mut error_info) == 1)
                .map(|_| error_info.error_code)
                .unwrap_or(0);
            let mut status = ZcanChannelStatus::default();
            let channel_status = self
                .device
                .lib
                .get::<FnReadChannelStatus>(b"ZCAN_ReadChannelStatus\0")
                .ok()
                .filter(|read| read(ch, &mut status) == 1)
                .map(|_| status);
            if let Some(status) = channel_status {
                let counters = (status.reg_re_counter, status.reg_te_counter);
                if self
                    .last_error_counters
                    .is_some_and(|previous| previous != counters)
                {
                    self.pending_error_frames.push(CanFrame {
                        t: self.start.elapsed().as_secs_f64(),
                        ch: 1,
                        tx: false,
                        id: 0,
                        ext: false,
                        fd: false,
                        brs: false,
                        remote: false,
                        error: true,
                        data: vec![0, status.reg_ec_capture, counters.0, counters.1],
                    });
                }
                self.last_error_counters = Some(counters);
            }
            if error_code == 0 {
                self.last_error_code = 0;
                return PollReport::default();
            }
            let is_new_error = error_code != self.last_error_code;
            self.last_error_code = error_code;
            let overflow = error_code
                & (ERROR_CAN_OVERFLOW | ERROR_CAN_BUFFER_OVERFLOW | ERROR_BUFFEROVERFLOW)
                != 0;
            let mut message = zcan_error_message(error_code, channel_status);
            if is_new_error {
                let status = channel_status.unwrap_or_default();
                let mut data = vec![
                    0,
                    status.reg_ec_capture,
                    status.reg_re_counter,
                    status.reg_te_counter,
                ];
                data.extend_from_slice(&error_code.to_le_bytes());
                self.pending_error_frames.push(CanFrame {
                    t: self.start.elapsed().as_secs_f64(),
                    ch: 1,
                    tx: false,
                    id: 8,
                    ext: false,
                    fd: false,
                    brs: false,
                    remote: false,
                    error: true,
                    data,
                });
            }

            if error_code & ERROR_CAN_BUSOFF != 0
                && self
                    .last_busoff_recovery
                    .is_none_or(|last| last.elapsed() >= Duration::from_secs(1))
            {
                self.last_busoff_recovery = Some(Instant::now());
                let reset_ok = self
                    .device
                    .lib
                    .get::<FnResetCan>(b"ZCAN_ResetCAN\0")
                    .is_ok_and(|reset| reset(ch) == 1);
                let clear_ok = self
                    .device
                    .lib
                    .get::<FnClearBuffer>(b"ZCAN_ClearBuffer\0")
                    .is_ok_and(|clear| clear(ch) == 1);
                let start_ok = self
                    .device
                    .lib
                    .get::<FnStartCan>(b"ZCAN_StartCAN\0")
                    .is_ok_and(|start| start(ch) == 1);
                message.push_str(if reset_ok && clear_ok && start_ok {
                    "；已自动复位并重新启动通道"
                } else {
                    "；自动恢复失败，请断开设备后重新连接"
                });
            }

            PollReport {
                receive_overruns: u64::from(overflow),
                driver_errors: u64::from(is_new_error),
                connection_lost: zcan_error_is_connection_lost(error_code),
                message: Some(message),
            }
        }
    }

    pub(super) fn transmit_error(&mut self, operation: &str) -> String {
        self.health_report(true)
            .message
            .map(|message| format!("{operation} 失败；{message}"))
            .unwrap_or_else(|| {
                format!(
                    "{operation} 失败：驱动未接收该帧，请检查通道模式、总线接线、终端电阻和节点 ACK"
                )
            })
    }
}

impl Drop for ZcanFdBus {
    fn drop(&mut self) {
        unsafe {
            if let Ok(reset) = self
                .device
                .lib
                .get::<zcan_ffi::FnResetCan>(b"ZCAN_ResetCAN\0")
            {
                let _ = reset(self.ch as zcan_ffi::ChHandle);
            }
            if let Ok(clear) = self
                .device
                .lib
                .get::<zcan_ffi::FnClearBuffer>(b"ZCAN_ClearBuffer\0")
            {
                let _ = clear(self.ch as zcan_ffi::ChHandle);
            }
        }
    }
}

impl CanAdapter for ZcanFdBus {
    fn poll(&mut self, out: &mut Vec<CanFrame>) -> PollReport {
        use zcan_ffi::*;
        let ch = self.ch as ChHandle;
        let mut report = self.health_report(false);
        for mut frame in self.pending_error_frames.drain(..) {
            frame.ch = 1;
            out.push(frame);
        }
        if report.connection_lost {
            return report;
        }
        unsafe {
            let getnum: FnGetReceiveNum = match self.device.lib.get(b"ZCAN_GetReceiveNum\0") {
                Ok(s) => *s,
                Err(error) => {
                    return PollReport {
                        driver_errors: 1,
                        connection_lost: true,
                        message: Some(format!("ZCAN_GetReceiveNum 未找到: {error}")),
                        ..Default::default()
                    };
                }
            };
            if let Ok(recv) = self
                .device
                .lib
                .get::<FnReceive>(b"ZCAN_Receive\0")
                .map(|s| *s)
            {
                let available = getnum(ch, TYPE_CAN);
                if available == u32::MAX {
                    report.driver_errors = report.driver_errors.saturating_add(1);
                    report.message = Some("ZCAN_GetReceiveNum(CAN) 返回驱动错误".into());
                    merge_poll_report(&mut report, self.health_report(true));
                } else if available > 0 {
                    let n = available.min(256);
                    let empty = ZcanReceiveData {
                        frame: CanFrameC {
                            can_id: 0,
                            can_dlc: 0,
                            pad: 0,
                            res0: 0,
                            res1: 0,
                            data: [0; 8],
                        },
                        timestamp: 0,
                    };
                    let mut buf = [empty; 256];
                    let received = recv(ch, buf.as_mut_ptr(), n, 0);
                    let got = if received == u32::MAX {
                        report.driver_errors = report.driver_errors.saturating_add(1);
                        report.message = Some("ZCAN_Receive 返回驱动错误".into());
                        merge_poll_report(&mut report, self.health_report(true));
                        0
                    } else {
                        received.min(n)
                    };
                    for r in buf.iter().take(got as usize) {
                        let len = (r.frame.can_dlc as usize).min(8);
                        let timestamp = self
                            .timestamp
                            .map(r.timestamp, self.start.elapsed().as_secs_f64());
                        out.push(CanFrame {
                            t: timestamp,
                            ch: 1,
                            tx: false,
                            id: r.frame.can_id & ID_MASK,
                            ext: r.frame.can_id & EFF != 0,
                            fd: false,
                            brs: false,
                            remote: r.frame.can_id & RTR != 0,
                            error: false,
                            data: r.frame.data[..len].to_vec(),
                        });
                    }
                }
            }
            if self.channel_is_fd
                && let Ok(recv) = self
                    .device
                    .lib
                    .get::<FnReceiveFd>(b"ZCAN_ReceiveFD\0")
                    .map(|s| *s)
            {
                let available = getnum(ch, TYPE_CANFD);
                if available == u32::MAX {
                    report.driver_errors = report.driver_errors.saturating_add(1);
                    report.message = Some("ZCAN_GetReceiveNum(CAN FD) 返回驱动错误".into());
                    merge_poll_report(&mut report, self.health_report(true));
                } else if available > 0 {
                    let n = available.min(256);
                    let empty = ZcanReceiveFdData {
                        frame: CanfdFrameC {
                            can_id: 0,
                            len: 0,
                            flags: 0,
                            res0: 0,
                            res1: 0,
                            data: [0; 64],
                        },
                        timestamp: 0,
                    };
                    let mut buf = [empty; 256];
                    let received = recv(ch, buf.as_mut_ptr(), n, 0);
                    let got = if received == u32::MAX {
                        report.driver_errors = report.driver_errors.saturating_add(1);
                        report.message = Some("ZCAN_ReceiveFD 返回驱动错误".into());
                        merge_poll_report(&mut report, self.health_report(true));
                        0
                    } else {
                        received.min(n)
                    };
                    for r in buf.iter().take(got as usize) {
                        let len = (r.frame.len as usize).min(64);
                        let timestamp = self
                            .timestamp
                            .map(r.timestamp, self.start.elapsed().as_secs_f64());
                        out.push(CanFrame {
                            t: timestamp,
                            ch: 1,
                            tx: false,
                            id: r.frame.can_id & ID_MASK,
                            ext: r.frame.can_id & EFF != 0,
                            fd: true,
                            brs: r.frame.flags & 0x01 != 0,
                            remote: r.frame.can_id & RTR != 0,
                            error: false,
                            data: r.frame.data[..len].to_vec(),
                        });
                    }
                }
            }
        }
        report
    }

    fn send(&mut self, f: &CanFrame) -> Result<(), String> {
        use zcan_ffi::*;
        if self.listen_only {
            return Err("监听模式禁止发送 CAN 报文".into());
        }
        if f.fd && !self.channel_is_fd {
            return Err("当前 ZLG 通道按 Classical CAN 初始化，不能发送 CAN FD 帧".into());
        }
        if !f.fd && f.data.len() > 8 {
            return Err("Classical CAN 数据长度不能超过 8 字节".into());
        }
        let ch = self.ch as ChHandle;
        let mut can_id = f.id & ID_MASK;
        if f.ext {
            can_id |= EFF;
        }
        if f.remote {
            can_id |= RTR;
        }
        unsafe {
            if f.fd {
                let transmit: FnTransmitFd = *self
                    .device
                    .lib
                    .get(b"ZCAN_TransmitFD\0")
                    .map_err(|e| format!("ZCAN_TransmitFD 未找到: {e}"))?;
                let mut data = [0u8; 64];
                let len = f.data.len().min(64);
                data[..len].copy_from_slice(&f.data[..len]);
                let msg = ZcanTransmitFdData {
                    frame: CanfdFrameC {
                        can_id,
                        len: len as u8,
                        flags: if f.brs { 0x01 } else { 0x00 },
                        res0: 0,
                        res1: 0,
                        data,
                    },
                    transmit_type: 0,
                };
                if transmit(ch, &msg, 1) != 1 {
                    return Err(self.transmit_error("ZCAN_TransmitFD"));
                }
            } else {
                let transmit: FnTransmit = *self
                    .device
                    .lib
                    .get(b"ZCAN_Transmit\0")
                    .map_err(|e| format!("ZCAN_Transmit 未找到: {e}"))?;
                let mut data = [0u8; 8];
                let len = f.data.len().min(8);
                data[..len].copy_from_slice(&f.data[..len]);
                let msg = ZcanTransmitData {
                    frame: CanFrameC {
                        can_id,
                        can_dlc: len as u8,
                        pad: 0,
                        res0: 0,
                        res1: 0,
                        data,
                    },
                    transmit_type: 0,
                };
                if transmit(ch, &msg, 1) != 1 {
                    return Err(self.transmit_error("ZCAN_Transmit"));
                }
            }
        }
        Ok(())
    }

    fn name(&self) -> &str {
        &self.name
    }
}
