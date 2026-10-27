//! `re-fs autostart`: mount the tablet whenever it's plugged in, from login on. It
//! registers re-fsw.exe (which runs re-fs.exe without a console window, logging to
//! %LOCALAPPDATA%\re-fs\re-fs.log) under HKCU\…\Run, and starts it right away.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use windows_sys::Win32::System::Registry::{RegDeleteKeyValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "re-fs";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn enable(mountpoint: &Path, mount_args: &[String]) -> Result<()> {
    let launcher = std::env::current_exe()?.with_file_name("re-fsw.exe");
    if !launcher.exists() {
        bail!("{} is missing; it comes with re-fs.exe and must sit next to it", launcher.display());
    }
    let mut args = vec!["mount".to_string(), mountpoint.to_string_lossy().into_owned(), "--usb".into(), "--follow-usb".into()];
    args.extend(mount_args.iter().cloned());
    let command = std::iter::once(launcher.display().to_string()).chain(args.iter().cloned()).map(|a| quote(&a)).collect::<Vec<_>>().join(" ");
    let (key, value, data) = (wide(RUN_KEY), wide(VALUE), wide(&command));
    // SAFETY: data is a NUL-terminated REG_SZ of the given byte length.
    let r = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32) };
    if r != 0 {
        bail!("registering the auto-mount: {}", std::io::Error::from_raw_os_error(r as i32));
    }
    stop_running(); // an earlier auto-mount, possibly with other arguments
    Command::new(&launcher).args(&args).spawn().with_context(|| format!("starting {}", launcher.display()))?;
    eprintln!("re-fs: the tablet now mounts on {} whenever it's plugged in by USB, from login on", mountpoint.display());
    eprintln!("re-fs: log: %LOCALAPPDATA%\\re-fs\\re-fs.log; turn off with `re-fs autostart --off`");
    Ok(())
}

pub fn disable() -> Result<()> {
    let (key, value) = (wide(RUN_KEY), wide(VALUE));
    // SAFETY: both are NUL-terminated.
    let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) };
    const ERROR_FILE_NOT_FOUND: u32 = 2;
    if r != 0 && r != ERROR_FILE_NOT_FOUND {
        bail!("removing the auto-mount: {}", std::io::Error::from_raw_os_error(r as i32));
    }
    stop_running();
    eprintln!("re-fs: auto-mount off");
    Ok(())
}

/// End a running auto-mount: re-fsw.exe and the re-fs.exe it started (/T), which
/// unmounts the drive.
fn stop_running() {
    let _ = Command::new("taskkill").args(["/F", "/T", "/IM", "re-fsw.exe"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// One argument for a Windows command line, quoted as CommandLineToArgvW reads it.
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_owned();
    }
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn quoting() {
        use super::quote;
        assert_eq!(quote("R:"), "R:");
        assert_eq!(quote(r"C:\Program Files\re-fs\re-fsw.exe"), r#""C:\Program Files\re-fs\re-fsw.exe""#);
        assert_eq!(quote(r#"a"b"#), r#""a\"b""#);
        assert_eq!(quote(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote(""), r#""""#);
    }
}
