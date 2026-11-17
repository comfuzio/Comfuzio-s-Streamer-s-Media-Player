//! Start at OS sign-in, launching into the tray (`--autostart`).
//!
//! - Windows: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, with the
//!   executable path quoted (install folders contain spaces).
//! - Linux: `~/.config/autostart/comfuzio-player.desktop`, pointing at the
//!   AppImage file itself (`$APPIMAGE`) rather than its temporary mount.
//! - macOS: a LaunchAgent via tauri-plugin-autostart.
//!
//! Applying is idempotent and re-run at every launch, so the entry follows the
//! app if it is reinstalled or moved.

use tauri::AppHandle;

/// Argument the OS passes when it launches the app at sign-in.
pub const AUTOSTART_ARG: &str = "--autostart";
#[cfg(any(windows, target_os = "linux"))]
const ENTRY_NAME: &str = "Comfuzio Streamers Media Player";

pub fn launched_by_autostart() -> bool {
    std::env::args().any(|a| a == AUTOSTART_ARG)
}

#[cfg(any(windows, target_os = "linux"))]
fn executable() -> Result<std::path::PathBuf, String> {
    // An AppImage runs from a temporary mount; $APPIMAGE is the real file.
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        return Ok(appimage.into());
    }
    std::env::current_exe().map_err(|e| format!("cannot locate the app: {e}"))
}

/// The exact command line stored in the OS entry.
#[cfg(any(windows, target_os = "linux", test))]
pub fn command_line(exe: &std::path::Path) -> String {
    format!("\"{}\" {AUTOSTART_ARG}", exe.display())
}

#[cfg(windows)]
pub fn set(_app: &AppHandle, enabled: bool) -> Result<(), String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let (run, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
        .map_err(|e| e.to_string())?;
    if enabled {
        run.set_value(ENTRY_NAME, &command_line(&executable()?)).map_err(|e| e.to_string())
    } else {
        match run.delete_value(ENTRY_NAME) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }
    }
}

#[cfg(target_os = "linux")]
pub fn set(_app: &AppHandle, enabled: bool) -> Result<(), String> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .ok_or("cannot find the config folder")?;
    let file = config.join("autostart").join("comfuzio-player.desktop");
    if enabled {
        std::fs::create_dir_all(file.parent().expect("has parent")).map_err(|e| e.to_string())?;
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName={ENTRY_NAME}\nExec={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
            command_line(&executable()?)
        );
        std::fs::write(&file, entry).map_err(|e| e.to_string())
    } else {
        match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }
    }
}

#[cfg(target_os = "macos")]
pub fn set(app: &AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let launcher = app.autolaunch();
    if launcher.is_enabled().unwrap_or(false) == enabled {
        return Ok(());
    }
    if enabled { launcher.enable() } else { launcher.disable() }.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_quotes_paths_with_spaces() {
        let exe = std::path::Path::new("C:/Users/x/AppData/Local/Comfuzio Streamers Media Player/comfuzio-player.exe");
        assert_eq!(
            command_line(exe),
            "\"C:/Users/x/AppData/Local/Comfuzio Streamers Media Player/comfuzio-player.exe\" --autostart"
        );
    }
}
