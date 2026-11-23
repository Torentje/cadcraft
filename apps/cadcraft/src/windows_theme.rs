//! Windows title bar dark mode configuration via Desktop Window Manager (DWM).
//!
//! Windows 10 (19041+) and Windows 11 require `DWMWA_USE_IMMERSIVE_DARK_MODE` (attribute 20)
//! to render the native title bar in dark mode with visible white caption buttons.
//! Without this, active windows render with a white title bar and white buttons (invisible).

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::num::NonZeroIsize;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE,
    DwmSetWindowAttribute,
};
use windows::Win32::UI::WindowsAndMessaging::{
    IsWindow, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
};

const DWMWA_USE_IMMERSIVE_DARK_MODE_OLD: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(19);

/// Apply dark or light theme to the native Windows title bar.
pub fn apply(hwnd: NonZeroIsize, dark: bool) {
    let win_hwnd = HWND(hwnd.get() as *mut c_void);
    let value: i32 = i32::from(dark);
    // SAFETY: `hwnd` is non-zero and verified via `IsWindow`; attribute pointers refer to
    // stack-allocated `i32`/`u32` values with exact `size_of` byte counts for the synchronous
    // Win32 call, and `SWP_NOACTIVATE` prevents unintended focus changes.
    unsafe {
        if !IsWindow(Some(win_hwnd)).as_bool() {
            return;
        }
        if DwmSetWindowAttribute(
            win_hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&raw const value).cast(),
            std::mem::size_of::<i32>() as u32,
        )
        .is_err()
        {
            // Attribute 19 fallback for Windows 10 (1809 - 1909).
            let _ = DwmSetWindowAttribute(
                win_hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE_OLD,
                (&raw const value).cast(),
                std::mem::size_of::<i32>() as u32,
            );
        }

        const DWMWA_COLOR_DEFAULT: u32 = 0xFFFF_FFFF;
        // Dark title bar (#202020) with white text on Windows 11; default (0xFFFFFFFF) on light theme.
        let (caption_color, text_color): (u32, u32) = if dark {
            (0x0020_2020, 0x00FF_FFFF)
        } else {
            (DWMWA_COLOR_DEFAULT, DWMWA_COLOR_DEFAULT)
        };
        let _ = DwmSetWindowAttribute(
            win_hwnd,
            DWMWA_CAPTION_COLOR,
            (&raw const caption_color).cast(),
            std::mem::size_of::<u32>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            win_hwnd,
            DWMWA_TEXT_COLOR,
            (&raw const text_color).cast(),
            std::mem::size_of::<u32>() as u32,
        );

        let _ = SetWindowPos(
            win_hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}
