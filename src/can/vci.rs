//! vci responsibilities extracted from src/can.rs.
use super::*;

#[allow(non_camel_case_types)]
pub(super) mod zlg_ffi {
    pub type FnOpenDevice = unsafe extern "system" fn(u32, u32, u32) -> u32;
    pub type FnCloseDevice = unsafe extern "system" fn(u32, u32) -> u32;
    pub type FnReadBoardInfo = unsafe extern "system" fn(u32, u32, *mut VCI_BOARD_INFO) -> u32;
    pub type FnInitCan = unsafe extern "system" fn(u32, u32, u32, *mut VCI_INIT_CONFIG) -> u32;
    pub type FnStartCan = unsafe extern "system" fn(u32, u32, u32) -> u32;
    pub type FnResetCan = unsafe extern "system" fn(u32, u32, u32) -> u32;
    pub type FnClearBuffer = unsafe extern "system" fn(u32, u32, u32) -> u32;
    pub type FnReadCanStatus = unsafe extern "system" fn(u32, u32, u32, *mut VCI_CAN_STATUS) -> u32;
    pub type FnReceive =
        unsafe extern "system" fn(u32, u32, u32, *mut VCI_CAN_OBJ, u32, i32) -> u32;
    pub type FnTransmit = unsafe extern "system" fn(u32, u32, u32, *mut VCI_CAN_OBJ, u32) -> u32;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct VCI_INIT_CONFIG {
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
    pub struct VCI_CAN_OBJ {
        pub id: u32,
        pub time_stamp: u32,
        pub time_flag: u8,
        pub send_type: u8,
        pub remote_flag: u8,
        pub extern_flag: u8,
        pub data_len: u8,
        pub data: [u8; 8],
        pub reserved: [u8; 3],
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct VCI_CAN_STATUS {
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

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct VCI_BOARD_INFO {
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

    impl Default for VCI_BOARD_INFO {
        fn default() -> Self {
            Self {
                hw_version: 0,
                fw_version: 0,
                driver_version: 0,
                interface_version: 0,
                irq_num: 0,
                can_num: 0,
                serial_number: [0; 20],
                hardware_type: [0; 40],
                reserved: [0; 4],
            }
        }
    }
}

#[cfg(windows)]
pub(super) mod win_pnp_ffi {
    use std::ffi::c_void;

    pub const DIGCF_PRESENT: u32 = 0x0000_0002;
    pub const DIGCF_ALLCLASSES: u32 = 0x0000_0004;

    #[repr(C)]
    pub struct SP_DEVINFO_DATA {
        pub cb_size: u32,
        pub class_guid: [u8; 16],
        pub dev_inst: u32,
        pub reserved: usize,
    }

    #[link(name = "setupapi")]
    unsafe extern "system" {
        pub fn SetupDiGetClassDevsW(
            class_guid: *const c_void,
            enumerator: *const u16,
            hwnd_parent: *mut c_void,
            flags: u32,
        ) -> *mut c_void;
        pub fn SetupDiEnumDeviceInfo(
            device_info_set: *mut c_void,
            member_index: u32,
            device_info_data: *mut SP_DEVINFO_DATA,
        ) -> i32;
        pub fn SetupDiGetDeviceInstanceIdW(
            device_info_set: *mut c_void,
            device_info_data: *mut SP_DEVINFO_DATA,
            device_instance_id: *mut u16,
            device_instance_id_size: u32,
            required_size: *mut u32,
        ) -> i32;
        pub fn SetupDiDestroyDeviceInfoList(device_info_set: *mut c_void) -> i32;
    }
}

#[cfg(windows)]
pub(super) fn windows_pnp_device_present(instance_prefix: &str) -> Option<bool> {
    use std::ffi::c_void;
    use win_pnp_ffi::*;

    unsafe {
        let devices = SetupDiGetClassDevsW(
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_ALLCLASSES,
        );
        if devices == (-1isize) as *mut c_void {
            return None;
        }
        let expected = instance_prefix.to_ascii_uppercase();
        let mut found = false;
        for index in 0..4096 {
            let mut info = SP_DEVINFO_DATA {
                cb_size: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
                class_guid: [0; 16],
                dev_inst: 0,
                reserved: 0,
            };
            if SetupDiEnumDeviceInfo(devices, index, &mut info) == 0 {
                break;
            }
            let mut buffer = [0u16; 512];
            let mut required = 0u32;
            if SetupDiGetDeviceInstanceIdW(
                devices,
                &mut info,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                &mut required,
            ) == 0
            {
                continue;
            }
            let length = buffer
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(buffer.len());
            let instance_id = String::from_utf16_lossy(&buffer[..length]).to_ascii_uppercase();
            if instance_id.starts_with(&expected) {
                found = true;
                break;
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(devices);
        Some(found)
    }
}

#[cfg(not(windows))]
pub(super) fn windows_pnp_device_present(_instance_prefix: &str) -> Option<bool> {
    None
}

pub(super) struct VciDevice {
    pub(super) lib: libloading::Library,
    pub(super) device_type: u32,
    pub(super) device_index: u32,
    pub(super) prefix: &'static str,
}

impl VciDevice {
    pub(super) fn sym(&self, name: &str) -> Vec<u8> {
        let mut value = format!("{}{name}", self.prefix).into_bytes();
        value.push(0);
        value
    }
}

impl Drop for VciDevice {
    fn drop(&mut self) {
        unsafe {
            if let Ok(close) = self
                .lib
                .get::<zlg_ffi::FnCloseDevice>(&*self.sym("CloseDevice"))
            {
                let _ = close(self.device_type, self.device_index);
            }
        }
    }
}

pub(super) type VciDeviceRegistry = HashMap<String, Arc<VciDevice>>;

pub(super) fn vci_device_registry() -> &'static Mutex<VciDeviceRegistry> {
    static DEVICES: std::sync::OnceLock<Mutex<VciDeviceRegistry>> = std::sync::OnceLock::new();
    DEVICES.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn vci_device_key(
    dll_candidates: &[&str],
    prefix: &str,
    device_type: u32,
    device_index: u32,
) -> String {
    format!(
        "{}|{prefix}|{device_type}|{device_index}",
        dll_candidates.first().copied().unwrap_or_default()
    )
}

pub(super) fn get_or_open_vci_device(
    dll_candidates: &[&str],
    prefix: &'static str,
    device_type: u32,
    device_index: u32,
) -> Result<(String, Arc<VciDevice>), String> {
    let key = vci_device_key(dll_candidates, prefix, device_type, device_index);
    let mut registry = vci_device_registry()
        .lock()
        .map_err(|_| "VCI 设备缓存已损坏".to_string())?;
    if let Some(device) = registry.get(&key) {
        return Ok((key, device.clone()));
    }
    unsafe {
        let mut loaded = None;
        let mut last_error = String::new();
        for candidate in dll_candidates {
            match libloading::Library::new(*candidate) {
                Ok(library) => {
                    loaded = Some(library);
                    break;
                }
                Err(error) => last_error = format!("加载 {candidate} 失败: {error}"),
            }
        }
        let lib = loaded.ok_or(last_error)?;
        let mut symbol = format!("{prefix}OpenDevice").into_bytes();
        symbol.push(0);
        let open = lib
            .get::<zlg_ffi::FnOpenDevice>(&*symbol)
            .map_err(|error| format!("{prefix}OpenDevice 未找到: {error}"))?;
        if open(device_type, device_index, 0) != 1 {
            return Err(format!("{prefix}OpenDevice 失败（检查设备/驱动）"));
        }
        drop(open);
        let device = Arc::new(VciDevice {
            lib,
            device_type,
            device_index,
            prefix,
        });
        registry.insert(key.clone(), device.clone());
        Ok((key, device))
    }
}

pub(super) fn evict_vci_device(key: &str, expected: &Arc<VciDevice>) {
    if let Ok(mut registry) = vci_device_registry().lock()
        && registry
            .get(key)
            .is_some_and(|cached| Arc::ptr_eq(cached, expected))
    {
        registry.remove(key);
    }
}

pub(super) fn clear_vci_device_registry() {
    if let Ok(mut registry) = vci_device_registry().lock() {
        registry.clear();
    }
}

pub struct VciBus {
    pub(super) device: Arc<VciDevice>,
    pub(super) channel_index: u32,
    pub(super) timing0: u8,
    pub(super) timing1: u8,
    pub(super) listen_only: bool,
    pub(super) start: Instant,
    pub(super) last_health_check: Instant,
    pub(super) last_physical_check: Instant,
    pub(super) last_busoff_recovery: Option<Instant>,
    pub(super) consecutive_send_failures: u8,
    pub(super) name: String,
}

impl VciBus {
    pub(super) fn open(
        start: Instant,
        cfg: &DeviceConfig,
        dll_candidates: &[&str],
        prefix: &'static str,
        device_type: u32,
    ) -> Result<Self, String> {
        use zlg_ffi::*;
        let (timing0, timing1) =
            zlg_timing(&cfg.baud).ok_or_else(|| format!("不支持的波特率: {}", cfg.baud))?;
        unsafe {
            let (key, device) =
                get_or_open_vci_device(dll_candidates, prefix, device_type, cfg.device_index)?;
            let setup = (|| -> Result<(), String> {
                let init = device
                    .lib
                    .get::<FnInitCan>(device.sym("InitCAN").as_slice())
                    .map_err(|error| format!("{prefix}InitCAN 未找到: {error}"))?;
                let mut init_cfg = VCI_INIT_CONFIG {
                    acc_code: 0,
                    acc_mask: 0xFFFF_FFFF,
                    reserved: 0,
                    filter: 1,
                    timing0,
                    timing1,
                    mode: u8::from(cfg.listen_only),
                };
                if init(
                    device_type,
                    cfg.device_index,
                    cfg.channel_index,
                    &mut init_cfg,
                ) != 1
                {
                    return Err(format!("{prefix}InitCAN 失败"));
                }
                drop(init);
                let start_can = device
                    .lib
                    .get::<FnStartCan>(device.sym("StartCAN").as_slice())
                    .map_err(|error| format!("{prefix}StartCAN 未找到: {error}"))?;
                if start_can(device_type, cfg.device_index, cfg.channel_index) != 1 {
                    return Err(format!("{prefix}StartCAN 失败"));
                }
                drop(start_can);
                if let Ok(clear) = device
                    .lib
                    .get::<FnClearBuffer>(device.sym("ClearBuffer").as_slice())
                {
                    let _ = clear(device_type, cfg.device_index, cfg.channel_index);
                }
                Ok(())
            })();
            if let Err(error) = setup {
                evict_vci_device(&key, &device);
                return Err(error);
            }
            Ok(Self {
                device,
                channel_index: cfg.channel_index,
                timing0,
                timing1,
                listen_only: cfg.listen_only,
                start,
                last_health_check: Instant::now() - Duration::from_secs(1),
                last_physical_check: Instant::now() - Duration::from_secs(1),
                last_busoff_recovery: None,
                consecutive_send_failures: 0,
                name: format!(
                    "{} dev{} CAN{} @{}",
                    cfg.device_type,
                    cfg.device_index,
                    cfg.channel_index,
                    normalize_baud(&cfg.baud)
                ),
            })
        }
    }

    pub(super) fn sym(&self, n: &str) -> Vec<u8> {
        self.device.sym(n)
    }

    pub(super) fn channel_status(&self) -> Result<Option<zlg_ffi::VCI_CAN_STATUS>, String> {
        unsafe {
            let Ok(read_status) = self
                .device
                .lib
                .get::<zlg_ffi::FnReadCanStatus>(self.sym("ReadCANStatus").as_slice())
            else {
                return Ok(None);
            };
            let mut status = zlg_ffi::VCI_CAN_STATUS::default();
            if read_status(
                self.device.device_type,
                self.device.device_index,
                self.channel_index,
                &mut status,
            ) != 1
            {
                return Err(format!("{}ReadCANStatus 失败", self.device.prefix));
            }
            Ok(Some(status))
        }
    }

    pub(super) fn device_present(&self) -> Result<(), String> {
        unsafe {
            let read_board_info = self
                .device
                .lib
                .get::<zlg_ffi::FnReadBoardInfo>(self.sym("ReadBoardInfo").as_slice())
                .map_err(|error| format!("{}ReadBoardInfo 未找到: {error}", self.device.prefix))?;
            let mut info = zlg_ffi::VCI_BOARD_INFO::default();
            if read_board_info(self.device.device_type, self.device.device_index, &mut info) != 1 {
                return Err(format!(
                    "{}设备物理连接已丢失（ReadBoardInfo 失败）",
                    self.device.prefix
                ));
            }
            Ok(())
        }
    }

    pub(super) fn recover_bus_off(&mut self) -> Result<(), String> {
        use zlg_ffi::*;
        unsafe {
            let reset = self
                .device
                .lib
                .get::<FnResetCan>(self.sym("ResetCAN").as_slice())
                .map_err(|error| format!("{}ResetCAN 未找到: {error}", self.device.prefix))?;
            if reset(
                self.device.device_type,
                self.device.device_index,
                self.channel_index,
            ) != 1
            {
                return Err(format!("{}ResetCAN 失败", self.device.prefix));
            }
            drop(reset);

            let mut init_cfg = VCI_INIT_CONFIG {
                acc_code: 0,
                acc_mask: 0xFFFF_FFFF,
                reserved: 0,
                filter: 1,
                timing0: self.timing0,
                timing1: self.timing1,
                mode: u8::from(self.listen_only),
            };
            let init = self
                .device
                .lib
                .get::<FnInitCan>(self.sym("InitCAN").as_slice())
                .map_err(|error| format!("{}InitCAN 未找到: {error}", self.device.prefix))?;
            if init(
                self.device.device_type,
                self.device.device_index,
                self.channel_index,
                &mut init_cfg,
            ) != 1
            {
                return Err(format!("{}InitCAN 恢复失败", self.device.prefix));
            }
            drop(init);

            let start = self
                .device
                .lib
                .get::<FnStartCan>(self.sym("StartCAN").as_slice())
                .map_err(|error| format!("{}StartCAN 未找到: {error}", self.device.prefix))?;
            if start(
                self.device.device_type,
                self.device.device_index,
                self.channel_index,
            ) != 1
            {
                return Err(format!("{}StartCAN 恢复失败", self.device.prefix));
            }
            drop(start);
            if let Ok(clear) = self
                .device
                .lib
                .get::<FnClearBuffer>(self.sym("ClearBuffer").as_slice())
            {
                let _ = clear(
                    self.device.device_type,
                    self.device.device_index,
                    self.channel_index,
                );
            }
        }
        Ok(())
    }
}

impl Drop for VciBus {
    fn drop(&mut self) {
        unsafe {
            if let Ok(reset) = self
                .device
                .lib
                .get::<zlg_ffi::FnResetCan>(self.sym("ResetCAN").as_slice())
            {
                let _ = reset(
                    self.device.device_type,
                    self.device.device_index,
                    self.channel_index,
                );
            }
        }
    }
}

impl CanAdapter for VciBus {
    fn poll(&mut self, out: &mut Vec<CanFrame>) -> PollReport {
        use zlg_ffi::*;
        unsafe {
            let recv: libloading::Symbol<FnReceive> =
                match self.device.lib.get(self.sym("Receive").as_slice()) {
                    Ok(s) => s,
                    Err(error) => {
                        return PollReport {
                            driver_errors: 1,
                            connection_lost: true,
                            message: Some(format!("{}Receive 未找到: {error}", self.device.prefix)),
                            ..Default::default()
                        };
                    }
                };
            let mut frames = [VCI_CAN_OBJ {
                id: 0,
                time_stamp: 0,
                time_flag: 0,
                send_type: 0,
                remote_flag: 0,
                extern_flag: 0,
                data_len: 0,
                data: [0; 8],
                reserved: [0; 3],
            }; 256];
            let received = recv(
                self.device.device_type,
                self.device.device_index,
                self.channel_index,
                frames.as_mut_ptr(),
                frames.len() as u32,
                0,
            );
            if received == u32::MAX {
                return PollReport {
                    driver_errors: 1,
                    connection_lost: true,
                    message: Some(format!("{}Receive 返回驱动错误", self.device.prefix)),
                    ..Default::default()
                };
            }
            let n = received.min(frames.len() as u32);
            for msg in frames.iter().take(n as usize) {
                let len = (msg.data_len as usize).min(8);
                // Both official legacy VCI headers expose TimeStamp/TimeFlag but
                // specify no clock unit. The attached GCAN and CANalyst-II drivers
                // returned different undocumented counter rates, so a common
                // 0.1 ms conversion produces invalid elapsed times.
                let timestamp = self.start.elapsed().as_secs_f64();
                out.push(CanFrame {
                    t: timestamp,
                    ch: (self.channel_index + 1) as u8,
                    tx: false,
                    id: msg.id,
                    ext: msg.extern_flag != 0,
                    fd: false,
                    brs: false,
                    remote: msg.remote_flag != 0,
                    error: false,
                    data: msg.data[..len].to_vec(),
                });
            }
        }
        if self.last_health_check.elapsed() < Duration::from_millis(250) {
            return PollReport::default();
        }
        self.last_health_check = Instant::now();
        if self.device.prefix.is_empty()
            && self.last_physical_check.elapsed() >= Duration::from_secs(1)
        {
            self.last_physical_check = Instant::now();
            if windows_pnp_device_present("USB\\VID_0C66&PID_000C\\") == Some(false) {
                return PollReport {
                    driver_errors: 1,
                    connection_lost: true,
                    message: Some("GCAN USB 设备已从 Windows PnP 设备树移除".into()),
                    ..Default::default()
                };
            }
        }
        if self.consecutive_send_failures >= 10 {
            match self.channel_status() {
                Ok(Some(status))
                    if status.reg_status == 0
                        && status.reg_re_counter == 0
                        && status.reg_te_counter == 0 =>
                {
                    return PollReport {
                        driver_errors: 1,
                        connection_lost: true,
                        message: Some(format!(
                            "{}设备连续发送失败且状态寄存器无响应，判定 USB 句柄已失效",
                            self.device.prefix
                        )),
                        ..Default::default()
                    };
                }
                Err(error) => {
                    return PollReport {
                        driver_errors: 1,
                        connection_lost: true,
                        message: Some(error),
                        ..Default::default()
                    };
                }
                _ => {}
            }
        }
        if let Err(error) = self.device_present() {
            return PollReport {
                driver_errors: 1,
                connection_lost: true,
                message: Some(error),
                ..Default::default()
            };
        }
        match self.channel_status() {
            Ok(Some(status)) => {
                let bus_off = status.reg_status & 0x80 != 0 || status.reg_te_counter == u8::MAX;
                let error_passive = status.reg_re_counter >= 128 || status.reg_te_counter >= 128;
                // CANalyst-II's ControlCAN driver was observed to stop at an
                // error counter of 135 while repeatedly rejecting transmission,
                // without ever exposing the SJA1000 Bus-Off status bit. Treat
                // that documented error-passive boundary as Bus-Off-equivalent
                // only when the transmit path also failed repeatedly.
                let recoverable_fault =
                    bus_off || (error_passive && self.consecutive_send_failures >= 10);
                let error_warning = status.reg_status & 0x40 != 0
                    || error_passive
                    || (status.reg_ew_limit != 0
                        && (status.reg_re_counter >= status.reg_ew_limit
                            || status.reg_te_counter >= status.reg_ew_limit));
                if recoverable_fault {
                    let can_recover = self
                        .last_busoff_recovery
                        .is_none_or(|last| last.elapsed() >= Duration::from_secs(1));
                    if can_recover {
                        self.last_busoff_recovery = Some(Instant::now());
                        let recovery = self.recover_bus_off();
                        if recovery.is_ok() {
                            self.consecutive_send_failures = 0;
                        }
                        let fault_name = if bus_off {
                            "Bus-Off"
                        } else {
                            "Bus-Off 等效错误被动（驱动未上报 Bus-Off 位）"
                        };
                        return match recovery {
                            Ok(()) => PollReport {
                                driver_errors: 1,
                                message: Some(format!(
                                    "{}CAN{} {fault_name}，已自动复位恢复（REC={} TEC={}）",
                                    self.device.prefix,
                                    self.channel_index + 1,
                                    status.reg_re_counter,
                                    status.reg_te_counter
                                )),
                                ..Default::default()
                            },
                            Err(error) => PollReport {
                                driver_errors: 1,
                                connection_lost: true,
                                message: Some(format!(
                                    "{}CAN{} {fault_name}恢复失败: {error}",
                                    self.device.prefix,
                                    self.channel_index + 1
                                )),
                                ..Default::default()
                            },
                        };
                    }
                } else if error_warning {
                    return PollReport {
                        driver_errors: 1,
                        message: Some(format!(
                            "{}CAN{} 总线错误警告（SR=0x{:02X} REC={} TEC={}）",
                            self.device.prefix,
                            self.channel_index + 1,
                            status.reg_status,
                            status.reg_re_counter,
                            status.reg_te_counter
                        )),
                        ..Default::default()
                    };
                }
                PollReport::default()
            }
            Ok(None) => PollReport::default(),
            Err(error) => PollReport {
                driver_errors: 1,
                connection_lost: true,
                message: Some(error),
                ..Default::default()
            },
        }
    }

    fn send(&mut self, f: &CanFrame) -> Result<(), String> {
        use zlg_ffi::*;
        if self.listen_only {
            return Err("监听模式禁止发送 CAN 报文".into());
        }
        if f.fd || f.data.len() > 8 {
            return Err("当前 VCI 适配器不支持 CAN FD 发送".into());
        }
        unsafe {
            let transmit: libloading::Symbol<FnTransmit> = self
                .device
                .lib
                .get(self.sym("Transmit").as_slice())
                .map_err(|e| format!("{}Transmit 未找到: {e}", self.device.prefix))?;
            let mut data = [0u8; 8];
            let len = f.data.len().min(8);
            data[..len].copy_from_slice(&f.data[..len]);
            let mut msg = VCI_CAN_OBJ {
                id: f.id,
                time_stamp: 0,
                time_flag: 0,
                send_type: 0,
                remote_flag: if f.remote { 1 } else { 0 },
                extern_flag: if f.ext { 1 } else { 0 },
                data_len: len as u8,
                data,
                reserved: [0; 3],
            };
            let transmitted = transmit(
                self.device.device_type,
                self.device.device_index,
                self.channel_index,
                &mut msg,
                1,
            );
            drop(transmit);
            if transmitted != 1 {
                self.consecutive_send_failures = self.consecutive_send_failures.saturating_add(1);
                let status = self.channel_status().ok().flatten();
                return Err(if let Some(status) = status {
                    format!(
                        "{}Transmit 失败（SR=0x{:02X} REC={} TEC={}）",
                        self.device.prefix,
                        status.reg_status,
                        status.reg_re_counter,
                        status.reg_te_counter
                    )
                } else {
                    format!("{}Transmit 失败", self.device.prefix)
                });
            }
            self.consecutive_send_failures = 0;
        }
        Ok(())
    }

    fn name(&self) -> &str {
        &self.name
    }
}
