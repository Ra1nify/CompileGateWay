//! Read the system accent color for the WebUI's pseudo-Mica background.
//!
//! On Windows it comes from the registry (`DWM\AccentColor`, stored as
//! `0xAABBGGRR`); elsewhere the notion does not exist, so `None` and the
//! frontend falls back to the dark-red default.

/// Returns `#rrggbb`, or `None` when unreadable.
pub fn accent_color() -> Option<String> {
    #[cfg(windows)]
    {
        windows_accent()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
fn windows_accent() -> Option<String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    // Try sources in priority order: AccentColor is the accent, ColorizationColor
    // the taskbar tint
    let candidates = [
        (r"Software\Microsoft\Windows\DWM", "AccentColor"),
        (r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent", "AccentColorMenu"),
        (r"Software\Microsoft\Windows\DWM", "ColorizationColor"),
    ];

    for (path, name) in candidates {
        let Ok(key) = hkcu.open_subkey(path) else {
            continue;
        };
        let Ok(value) = key.get_value::<u32, _>(name) else {
            continue;
        };
        // 0xAABBGGRR
        let r = (value & 0xFF) as u8;
        let g = ((value >> 8) & 0xFF) as u8;
        let b = ((value >> 16) & 0xFF) as u8;
        // All black / fully transparent counts as unset
        if r == 0 && g == 0 && b == 0 {
            continue;
        }
        return Some(format!("#{r:02x}{g:02x}{b:02x}"));
    }

    None
}
