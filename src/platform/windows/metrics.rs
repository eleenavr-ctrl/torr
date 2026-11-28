use crate::platform::{ProcessMetrics, ProcessMetricsSnapshot};
use std::time::Instant;

pub struct WindowsProcessMetrics {
    last_proc_100ns: u64,
    last_wall: Instant,
    cpu_cores: usize,
}

impl Default for WindowsProcessMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsProcessMetrics {
    pub fn new() -> Self {
        Self {
            last_proc_100ns: read_process_cpu_100ns().unwrap_or(0),
            last_wall: Instant::now(),
            cpu_cores: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
                .max(1),
        }
    }
}

impl ProcessMetrics for WindowsProcessMetrics {
    fn sample(&mut self) -> ProcessMetricsSnapshot {
        let mut snap = ProcessMetricsSnapshot::default();
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::ProcessStatus::{
                GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
            };
            use windows_sys::Win32::System::Threading::{
                GetCurrentProcess, GetGuiResources, GetProcessHandleCount, GR_GDIOBJECTS,
                GR_USEROBJECTS,
            };

            unsafe {
                let proc_handle = GetCurrentProcess();
                let mut pmc_ex: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
                pmc_ex.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
                if GetProcessMemoryInfo(
                    proc_handle,
                    &mut pmc_ex as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
                    pmc_ex.cb,
                ) != 0
                {
                    snap.working_set_bytes = pmc_ex.WorkingSetSize as u64;
                    snap.private_bytes = pmc_ex.PrivateUsage as u64;
                }

                let mut handles: u32 = 0;
                if GetProcessHandleCount(proc_handle, &mut handles) != 0 {
                    snap.handle_count = handles;
                }
                snap.gdi_objects = GetGuiResources(proc_handle, GR_GDIOBJECTS);
                snap.user_objects = GetGuiResources(proc_handle, GR_USEROBJECTS);
            }

            if let Some(now_100ns) = read_process_cpu_100ns() {
                let elapsed = self.last_wall.elapsed().as_secs_f64();
                if elapsed > 0.05 && now_100ns >= self.last_proc_100ns {
                    let delta_sec = (now_100ns - self.last_proc_100ns) as f64 / 10_000_000.0;
                    let pct = (delta_sec / (elapsed * self.cpu_cores as f64)) * 100.0;
                    snap.cpu_percent = pct.clamp(0.0, 100.0) as f32;
                }
                self.last_proc_100ns = now_100ns;
                self.last_wall = Instant::now();
            }
        }
        snap
    }
}

#[cfg(windows)]
fn read_process_cpu_100ns() -> Option<u64> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    unsafe {
        let mut creation: FILETIME = std::mem::zeroed();
        let mut exit: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        if GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        ) != 0
        {
            let k = ((kernel.dwHighDateTime as u64) << 32) | (kernel.dwLowDateTime as u64);
            let u = ((user.dwHighDateTime as u64) << 32) | (user.dwLowDateTime as u64);
            return Some(k.saturating_add(u));
        }
    }
    None
}

#[cfg(not(windows))]
fn read_process_cpu_100ns() -> Option<u64> {
    None
}
