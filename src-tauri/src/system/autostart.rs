//! Launch at login (spec 8, `system.launch_at_login`).
//!
//! Hand-written instead of `tauri-plugin-autostart`: it is one registry
//! value on Windows and one plist file on macOS, needs no new crate (the
//! plugin brings `auto-launch` plus its own registry/plist dependencies and
//! a JS command surface the app does not use), and the entry name is the
//! app identifier, so a dev build with a different identifier never touches
//! the installed app's entry.
//!
//! - **Windows:** a string value named after the app identifier under
//!   `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, holding the quoted
//!   path of the running executable. Off deletes that one value; other
//!   values in the key are never read or changed.
//! - **macOS:** a LaunchAgent `~/Library/LaunchAgents/<identifier>.plist`
//!   with `RunAtLoad` and the running executable as the program. launchd
//!   reads it at the next login; nothing is loaded now. Off deletes the file.
//!
//! The settings are the source of truth: at startup the entry is written or
//! removed to match them (this also repairs an entry that points at an old
//! executable path after the app was moved or updated).

use std::path::{Path, PathBuf};

/// Applies `enabled` for the running executable.
pub trait Autostart: Send + Sync {
    /// Registers (`true`) or removes (`false`) the login entry. Idempotent.
    fn apply(&self, enabled: bool) -> std::io::Result<()>;
}

/// The registry value data (Windows) for `exe`: the path in quotes, so a
/// path with spaces is one argument.
pub fn run_value_data(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

/// Registry key (under `HKEY_CURRENT_USER`) that holds the login entries.
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// The LaunchAgent plist path (macOS) for `identifier` under `home`.
pub fn launch_agent_path(home: &Path, identifier: &str) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{identifier}.plist"))
}

/// The LaunchAgent plist (macOS) that starts `exe` at login.
pub fn launch_agent_plist(identifier: &str, exe: &Path) -> String {
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
            "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
            "<plist version=\"1.0\">\n",
            "<dict>\n",
            "  <key>Label</key>\n",
            "  <string>{label}</string>\n",
            "  <key>ProgramArguments</key>\n",
            "  <array>\n",
            "    <string>{exe}</string>\n",
            "  </array>\n",
            "  <key>RunAtLoad</key>\n",
            "  <true/>\n",
            "  <key>ProcessType</key>\n",
            "  <string>Interactive</string>\n",
            "</dict>\n",
            "</plist>\n",
        ),
        label = xml_escape(identifier),
        exe = xml_escape(&exe.to_string_lossy()),
    )
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// The OS implementation for this platform.
pub struct OsAutostart {
    /// App identifier: the registry value name / LaunchAgent label.
    pub identifier: String,
    /// Executable to start at login. Empty if it could not be resolved;
    /// registering then fails (removing still works).
    pub exe: PathBuf,
}

impl OsAutostart {
    fn require_exe(&self) -> std::io::Result<()> {
        if self.exe.as_os_str().is_empty() {
            Err(std::io::Error::other("the executable path is unknown"))
        } else {
            Ok(())
        }
    }
}

#[cfg(windows)]
impl Autostart for OsAutostart {
    fn apply(&self, enabled: bool) -> std::io::Result<()> {
        if enabled {
            self.require_exe()?;
            windows_run::set(&self.identifier, &run_value_data(&self.exe))
        } else {
            windows_run::delete(&self.identifier)
        }
    }
}

#[cfg(target_os = "macos")]
impl Autostart for OsAutostart {
    fn apply(&self, enabled: bool) -> std::io::Result<()> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| std::io::Error::other("HOME is not set"))?;
        let path = launch_agent_path(&home, &self.identifier);
        if enabled {
            self.require_exe()?;
            let plist = launch_agent_plist(&self.identifier, &self.exe);
            if std::fs::read_to_string(&path).is_ok_and(|current| current == plist) {
                return Ok(());
            }
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, plist)
        } else {
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
impl Autostart for OsAutostart {
    fn apply(&self, _enabled: bool) -> std::io::Result<()> {
        let _ = self.require_exe();
        Err(std::io::Error::other(
            "launch at login is not supported on this platform",
        ))
    }
}

/// `HKCU\...\Run` value access through the `windows` crate (already a
/// dependency; only the `Win32_System_Registry` feature is added).
#[cfg(windows)]
pub mod windows_run {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
    use windows::Win32::System::Registry::{
        RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
    };

    use super::RUN_KEY;

    fn check(err: WIN32_ERROR) -> std::io::Result<()> {
        if err.is_ok() {
            Ok(())
        } else {
            Err(std::io::Error::from_raw_os_error(err.0 as i32))
        }
    }

    /// Reads our value, `None` if it does not exist.
    pub fn get(name: &str) -> std::io::Result<Option<String>> {
        let key = HSTRING::from(RUN_KEY);
        let name = HSTRING::from(name);
        let mut size = 0u32;
        // SAFETY: valid strings; a null data pointer asks for the size.
        let err = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(key.as_ptr()),
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                None,
                Some(&mut size),
            )
        };
        if err == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check(err)?;
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        // SAFETY: `buf` holds `size` bytes.
        let err = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(key.as_ptr()),
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if err == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check(err)?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Ok(Some(String::from_utf16_lossy(&buf[..len])))
    }

    /// Writes our value (skipped if it already holds `data`).
    pub fn set(name: &str, data: &str) -> std::io::Result<()> {
        if get(name)?.as_deref() == Some(data) {
            return Ok(());
        }
        let key = HSTRING::from(RUN_KEY);
        let value = HSTRING::from(name);
        let wide: Vec<u16> = data.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = u32::try_from(wide.len() * 2).map_err(std::io::Error::other)?;
        // SAFETY: valid strings; `wide` is a NUL-terminated UTF-16 string of
        // `bytes` bytes.
        check(unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                PCWSTR(key.as_ptr()),
                PCWSTR(value.as_ptr()),
                REG_SZ.0,
                Some(wide.as_ptr().cast()),
                bytes,
            )
        })
    }

    /// Deletes our value; a missing value is not an error.
    pub fn delete(name: &str) -> std::io::Result<()> {
        let key = HSTRING::from(RUN_KEY);
        let value = HSTRING::from(name);
        // SAFETY: valid strings.
        let err = unsafe {
            RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                PCWSTR(key.as_ptr()),
                PCWSTR(value.as_ptr()),
            )
        };
        if err == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        check(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_value_quotes_the_path() {
        let exe = Path::new(r"C:\Program Files\AIchemist Buddy\aichemist-desktop-buddy.exe");
        assert_eq!(
            run_value_data(exe),
            r#""C:\Program Files\AIchemist Buddy\aichemist-desktop-buddy.exe""#
        );
    }

    #[test]
    fn launch_agent_lives_in_the_user_library() {
        let path = launch_agent_path(Path::new("/Users/me"), "io.github.example.app");
        assert_eq!(
            path,
            Path::new("/Users/me")
                .join("Library")
                .join("LaunchAgents")
                .join("io.github.example.app.plist")
        );
    }

    #[test]
    fn launch_agent_plist_runs_the_exe_at_load_and_escapes() {
        let plist = launch_agent_plist(
            "io.github.example.app",
            Path::new("/Applications/A & B <x>.app/Contents/MacOS/app"),
        );
        assert!(plist.contains("<string>io.github.example.app</string>"));
        assert!(plist
            .contains("<string>/Applications/A &amp; B &lt;x&gt;.app/Contents/MacOS/app</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(plist.starts_with("<?xml"));
    }

    /// Writes, reads and deletes a value under a throwaway name in the real
    /// Run key; never touches any other value.
    #[cfg(windows)]
    #[test]
    fn windows_run_value_round_trip() {
        let name = format!("aichemist-autostart-test-{}", std::process::id());
        let data = run_value_data(Path::new(r"C:\x y\z.exe"));
        windows_run::delete(&name).unwrap();
        assert_eq!(windows_run::get(&name).unwrap(), None);
        windows_run::set(&name, &data).unwrap();
        assert_eq!(windows_run::get(&name).unwrap().as_deref(), Some(&*data));
        windows_run::set(&name, &data).unwrap(); // idempotent
        windows_run::delete(&name).unwrap();
        assert_eq!(windows_run::get(&name).unwrap(), None);
        windows_run::delete(&name).unwrap(); // missing is fine
    }
}
