//! Deadline-based waiting for the CAN controller loop.
use std::time::Instant;

#[cfg(windows)]
pub(super) struct PreciseWaiter {
    handle: *mut std::ffi::c_void,
}

#[cfg(windows)]
impl PreciseWaiter {
    pub(super) fn new() -> Self {
        let handle = unsafe {
            let high_resolution = CreateWaitableTimerExW(
                std::ptr::null_mut(),
                std::ptr::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS,
            );
            if high_resolution.is_null() {
                CreateWaitableTimerExW(std::ptr::null_mut(), std::ptr::null(), 0, TIMER_ALL_ACCESS)
            } else {
                high_resolution
            }
        };
        Self { handle }
    }

    pub(super) fn wait_until(&self, deadline: Instant) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        if self.handle.is_null() {
            std::thread::sleep(remaining);
            return;
        }
        let ticks_100ns = remaining.as_nanos().div_ceil(100).min(i64::MAX as u128) as i64;
        let due_time = -ticks_100ns.max(1);
        let armed =
            unsafe { SetWaitableTimer(self.handle, &due_time, 0, None, std::ptr::null_mut(), 0) };
        if armed != 0 {
            unsafe {
                WaitForSingleObject(self.handle, INFINITE);
            }
        } else {
            std::thread::sleep(remaining);
        }
    }
}

#[cfg(windows)]
impl Drop for PreciseWaiter {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(windows)]
const CREATE_WAITABLE_TIMER_HIGH_RESOLUTION: u32 = 0x0000_0002;
#[cfg(windows)]
const TIMER_ALL_ACCESS: u32 = 0x001F_0003;
#[cfg(windows)]
const INFINITE: u32 = 0xFFFF_FFFF;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateWaitableTimerExW(
        timer_attributes: *mut std::ffi::c_void,
        timer_name: *const u16,
        flags: u32,
        desired_access: u32,
    ) -> *mut std::ffi::c_void;
    fn SetWaitableTimer(
        timer: *mut std::ffi::c_void,
        due_time: *const i64,
        period_ms: i32,
        completion_routine: Option<unsafe extern "system" fn(*mut std::ffi::c_void, u32, u32)>,
        completion_arg: *mut std::ffi::c_void,
        resume: i32,
    ) -> i32;
    fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
}

#[cfg(not(windows))]
pub(super) struct PreciseWaiter;

#[cfg(not(windows))]
impl PreciseWaiter {
    pub(super) fn new() -> Self {
        Self
    }

    pub(super) fn wait_until(&self, deadline: Instant) {
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
    }
}
