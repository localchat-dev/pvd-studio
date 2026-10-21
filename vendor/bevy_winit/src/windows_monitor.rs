use core::mem::{size_of, zeroed};
use std::io;

use bevy_math::{IVec2, UVec2};
use bevy_window::{Monitor, VideoMode};
use windows_sys::Win32::{
    Graphics::Gdi::{
        DEVMODEW, DM_BITSPERPEL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH,
        ENUM_CURRENT_SETTINGS, EnumDisplaySettingsExW, GetMonitorInfoW, HMONITOR, MONITORINFO,
        MONITORINFOEXW,
    },
    UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
};

const MAX_VIDEO_MODES: u32 = 4096;

pub(super) struct Snapshot {
    handle: HMONITOR,
    info: MONITORINFOEXW,
}

impl Snapshot {
    pub(super) fn query(handle: HMONITOR) -> io::Result<Self> {
        // SAFETY: MONITORINFOEXW is a plain Win32 output structure. Its size is set before FFI.
        let mut info: MONITORINFOEXW = unsafe { zeroed() };
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: The output pointer references the complete initialized structure. Windows
        // validates the opaque handle and reports invalid or disconnected monitors as failure.
        if unsafe { GetMonitorInfoW(handle, core::ptr::from_mut(&mut info).cast::<MONITORINFO>()) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        geometry(&info)?;
        Ok(Self { handle, info })
    }

    pub(super) fn into_monitor(self) -> io::Result<Monitor> {
        let (size, position, name) = geometry(&self.info)?;
        let mut dpi_x = 0;
        let mut dpi_y = 0;
        // SAFETY: Both output pointers reference initialized u32 values. Handle lifetime is
        // controlled by Windows, so a failure between the snapshot and DPI query is propagated.
        let dpi_result =
            unsafe { GetDpiForMonitor(self.handle, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
        if dpi_result < 0 || dpi_x == 0 || dpi_y == 0 {
            return Err(io::Error::other(format!(
                "GetDpiForMonitor failed (HRESULT {dpi_result:#x}, DPI {dpi_x}/{dpi_y})"
            )));
        }

        let current = self
            .display_mode(ENUM_CURRENT_SETTINGS)
            .ok_or_else(|| io::Error::other("current display mode is unavailable"))?;
        let refresh_rate_millihertz = Some(convert_mode(&current)?.refresh_rate_millihertz);
        let mut video_modes = Vec::new();
        let mut exhausted = false;
        for index in 0..MAX_VIDEO_MODES {
            let Some(mode) = self.display_mode(index) else {
                exhausted = true;
                break;
            };
            video_modes.push(convert_mode(&mode)?);
        }
        if !exhausted {
            return Err(io::Error::other(
                "display mode enumeration exceeds 4096 entries",
            ));
        }
        video_modes.sort_unstable_by_key(|mode| {
            (
                mode.physical_size.x,
                mode.physical_size.y,
                mode.bit_depth,
                mode.refresh_rate_millihertz,
            )
        });
        video_modes.dedup();
        Ok(Monitor {
            name: Some(name),
            physical_width: size.x,
            physical_height: size.y,
            physical_position: position,
            refresh_rate_millihertz,
            scale_factor: f64::from(dpi_x) / 96.0,
            video_modes,
        })
    }

    fn display_mode(&self, index: u32) -> Option<DEVMODEW> {
        // SAFETY: DEVMODEW is a plain Win32 output structure. The device string comes from the
        // validated snapshot and has a terminating NUL within its fixed-size array.
        let mut mode: DEVMODEW = unsafe { zeroed() };
        mode.dmSize = size_of::<DEVMODEW>() as u16;
        // SAFETY: Both pointers are valid for this synchronous call. Mode queries use the
        // snapshot's device name and never re-query a potentially expired HMONITOR.
        (unsafe { EnumDisplaySettingsExW(self.info.szDevice.as_ptr(), index, &mut mode, 0) } != 0)
            .then_some(mode)
    }
}

fn geometry(info: &MONITORINFOEXW) -> io::Result<(UVec2, IVec2, String)> {
    let rect = info.monitorInfo.rcMonitor;
    let width = i64::from(rect.right) - i64::from(rect.left);
    let height = i64::from(rect.bottom) - i64::from(rect.top);
    if !(1..=i64::from(u32::MAX)).contains(&width) || !(1..=i64::from(u32::MAX)).contains(&height) {
        return Err(io::Error::other(
            "monitor rectangle has no valid physical extent",
        ));
    }
    let end = info
        .szDevice
        .iter()
        .position(|unit| *unit == 0)
        .filter(|end| *end > 0)
        .ok_or_else(|| io::Error::other("monitor device name is empty or unterminated"))?;
    Ok((
        UVec2::new(width as u32, height as u32),
        IVec2::new(rect.left, rect.top),
        String::from_utf16_lossy(&info.szDevice[..end]),
    ))
}

fn convert_mode(mode: &DEVMODEW) -> io::Result<VideoMode> {
    const REQUIRED: u32 = DM_BITSPERPEL | DM_DISPLAYFREQUENCY | DM_PELSWIDTH | DM_PELSHEIGHT;
    if mode.dmFields & REQUIRED != REQUIRED || mode.dmPelsWidth == 0 || mode.dmPelsHeight == 0 {
        return Err(io::Error::other(
            "display mode has incomplete physical fields",
        ));
    }
    Ok(VideoMode {
        physical_size: UVec2::new(mode.dmPelsWidth, mode.dmPelsHeight),
        bit_depth: u16::try_from(mode.dmBitsPerPel)
            .map_err(|_| io::Error::other("display mode bit depth exceeds u16"))?,
        refresh_rate_millihertz: mode
            .dmDisplayFrequency
            .checked_mul(1000)
            .ok_or_else(|| io::Error::other("display refresh rate exceeds u32 millihertz"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::RECT;

    fn monitor_info() -> MONITORINFOEXW {
        // SAFETY: The fixture initializes the required fields of a plain Win32 structure.
        let mut info: MONITORINFOEXW = unsafe { zeroed() };
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        info.monitorInfo.rcMonitor = RECT {
            left: -1920,
            top: -120,
            right: 0,
            bottom: 960,
        };
        for (target, unit) in info
            .szDevice
            .iter_mut()
            .zip("\\\\.\\DISPLAY2".encode_utf16())
        {
            *target = unit;
        }
        info
    }

    #[test]
    fn invalid_native_handles_are_fallible() {
        assert!(Snapshot::query(0).is_err());
        assert!(Snapshot::query(-1).is_err());
    }

    #[test]
    fn handle_expiring_after_snapshot_is_fallible() {
        let snapshot = Snapshot {
            handle: 0,
            info: monitor_info(),
        };
        assert!(snapshot.into_monitor().is_err());
    }

    #[test]
    fn physical_geometry_preserves_negative_desktop_positions() {
        let (size, position, name) = geometry(&monitor_info()).unwrap();
        assert_eq!(size, UVec2::new(1920, 1080));
        assert_eq!(position, IVec2::new(-1920, -120));
        assert_eq!(name, "\\\\.\\DISPLAY2");
    }

    #[test]
    fn malformed_monitor_metadata_is_rejected() {
        let mut info = monitor_info();
        info.monitorInfo.rcMonitor.right = info.monitorInfo.rcMonitor.left;
        assert!(geometry(&info).is_err());
        info = monitor_info();
        info.szDevice.fill(1);
        assert!(geometry(&info).is_err());
        info.szDevice.fill(0);
        assert!(geometry(&info).is_err());
    }

    #[test]
    fn mode_conversion_checks_required_fields_and_numeric_bounds() {
        // SAFETY: The fixture initializes the required fields of a plain Win32 structure.
        let mut mode: DEVMODEW = unsafe { zeroed() };
        mode.dmFields = DM_BITSPERPEL | DM_DISPLAYFREQUENCY | DM_PELSWIDTH | DM_PELSHEIGHT;
        mode.dmPelsWidth = 1920;
        mode.dmPelsHeight = 1080;
        mode.dmBitsPerPel = 32;
        mode.dmDisplayFrequency = 144;
        let converted = convert_mode(&mode).unwrap();
        assert_eq!(converted.physical_size, UVec2::new(1920, 1080));
        assert_eq!(converted.bit_depth, 32);
        assert_eq!(converted.refresh_rate_millihertz, 144_000);
        mode.dmDisplayFrequency = u32::MAX;
        assert!(convert_mode(&mode).is_err());
        mode.dmDisplayFrequency = 60;
        mode.dmFields &= !DM_DISPLAYFREQUENCY;
        assert!(convert_mode(&mode).is_err());
    }

    #[test]
    #[ignore = "requires an attached Windows desktop monitor; performs read-only Win32 queries"]
    fn attached_primary_monitor_has_real_metrics() {
        use windows_sys::Win32::{
            Foundation::POINT,
            Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint},
        };
        // SAFETY: This call only queries the primary monitor and changes no display state.
        let handle = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
        let monitor = Snapshot::query(handle).unwrap().into_monitor().unwrap();
        assert!(monitor.physical_width > 0 && monitor.physical_height > 0);
        assert!(monitor.scale_factor > 0.0);
        assert!(monitor.refresh_rate_millihertz.is_some());
        assert!(!monitor.video_modes.is_empty());
    }
}
