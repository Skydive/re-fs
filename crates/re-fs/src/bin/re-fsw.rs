//! re-fsw.exe: runs re-fs.exe (from the same folder) with the given arguments and
//! no console window, its output going to %LOCALAPPDATA%\re-fs\re-fs.log. It's what
//! `re-fs autostart` registers to start at login. Windows only.

#![windows_subsystem = "windows"]

fn main() {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::path::PathBuf;
        use std::process::{Command, Stdio};

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let Ok(exe) = std::env::current_exe() else { return };
        let dir = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("re-fs");
        let _ = std::fs::create_dir_all(&dir);
        let mut cmd = Command::new(exe.with_file_name("re-fs.exe"));
        cmd.args(std::env::args_os().skip(1)).creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null());
        // The log of the current run only.
        if let Ok(log) = std::fs::File::create(dir.join("re-fs.log")) {
            if let Ok(out) = log.try_clone() {
                cmd.stdout(out);
            }
            cmd.stderr(log);
        }
        if let Ok(mut child) = cmd.spawn() {
            let _ = child.wait();
        }
    }
}
