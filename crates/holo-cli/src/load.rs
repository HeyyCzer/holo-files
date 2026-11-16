//! How the PC is being used, for adaptive concurrency: CPU use, time since the last
//! keyboard/mouse input, and whether we're on battery.

use std::sync::Arc;

use holo_engine::adaptive::LoadProbe;

pub fn probe() -> Option<Arc<dyn LoadProbe>> {
    #[cfg(windows)]
    {
        Some(Arc::new(win::WindowsLoad::default()))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
mod win {
    use std::sync::Mutex;
    use std::time::Duration;

    use holo_engine::adaptive::{LoadProbe, SystemLoad};
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::System::Threading::GetSystemTimes;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

    /// CPU use is a delta between samples, so the previous one is kept.
    #[derive(Default)]
    pub struct WindowsLoad {
        last: Mutex<Option<CpuTimes>>,
    }

    #[derive(Clone, Copy)]
    struct CpuTimes {
        idle: u64,
        total: u64,
    }

    fn ticks(f: FILETIME) -> u64 {
        (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)
    }

    fn cpu_times() -> Option<CpuTimes> {
        let (mut idle, mut kernel, mut user) = Default::default();
        // SAFETY: out-pointers to locals.
        unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
        // Kernel time includes idle time.
        Some(CpuTimes {
            idle: ticks(idle),
            total: ticks(kernel) + ticks(user),
        })
    }

    fn user_idle() -> Option<Duration> {
        let mut info = LASTINPUTINFO {
            cbSize: size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // SAFETY: `info` is a properly sized local.
        if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
            return None;
        }
        // SAFETY: no arguments. Both values are u32 ms ticks; wrapping_sub handles rollover.
        let now = unsafe { GetTickCount() };
        Some(Duration::from_millis(u64::from(
            now.wrapping_sub(info.dwTime),
        )))
    }

    fn on_battery() -> bool {
        let mut s = SYSTEM_POWER_STATUS::default();
        // SAFETY: out-pointer to a local. ACLineStatus: 0 offline, 1 online, 255 unknown.
        unsafe { GetSystemPowerStatus(&mut s) }.is_ok() && s.ACLineStatus == 0
    }

    impl LoadProbe for WindowsLoad {
        fn sample(&self) -> Option<SystemLoad> {
            let now = cpu_times()?;
            let prev = self.last.lock().ok()?.replace(now);
            let cpu_busy = match prev {
                Some(p) if now.total > p.total => {
                    let total = (now.total - p.total) as f64;
                    let idle = now.idle.saturating_sub(p.idle) as f64;
                    (1.0 - idle / total).clamp(0.0, 1.0) as f32
                }
                _ => 0.0,
            };
            Some(SystemLoad {
                cpu_busy,
                user_idle: user_idle()?,
                on_battery: on_battery(),
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn samples_are_sane() {
            let probe = WindowsLoad::default();
            let _ = probe.sample();
            std::thread::sleep(Duration::from_millis(50));
            let s = probe.sample().expect("sample");
            assert!((0.0..=1.0).contains(&s.cpu_busy), "{s:?}");
        }
    }
}
