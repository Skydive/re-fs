//! The error numbers writes and reads report. FUSE passes them to the kernel as they
//! are; on Windows (no errno) they're Linux's numbers, which the WinFsp backend maps
//! to NTSTATUS.

// Some are only used by the WinFsp backend.
#[cfg(unix)]
#[allow(unused_imports)]
pub use libc::{EACCES, EAGAIN, EBADF, EBUSY, EEXIST, EHOSTDOWN, EINVAL, EIO, EISDIR, ENOENT, ENOTDIR, ENOTEMPTY, EPERM, EROFS, ETIMEDOUT};

#[cfg(not(unix))]
mod linux {
    pub const EPERM: i32 = 1;
    pub const ENOENT: i32 = 2;
    pub const EIO: i32 = 5;
    pub const EBADF: i32 = 9;
    pub const EAGAIN: i32 = 11;
    pub const EACCES: i32 = 13;
    pub const EBUSY: i32 = 16;
    pub const EEXIST: i32 = 17;
    pub const ENOTDIR: i32 = 20;
    pub const EISDIR: i32 = 21;
    pub const EINVAL: i32 = 22;
    pub const EROFS: i32 = 30;
    pub const ENOTEMPTY: i32 = 39;
    pub const ETIMEDOUT: i32 = 110;
    pub const EHOSTDOWN: i32 = 112;
}
#[cfg(not(unix))]
pub use linux::*;

pub type Errno = i32;

/// An io::Error as an errno (on Windows its OS code isn't one, so it's EIO).
pub fn of_io(e: &std::io::Error) -> Errno {
    #[cfg(unix)]
    return e.raw_os_error().unwrap_or(EIO);
    #[cfg(not(unix))]
    {
        let _ = e;
        EIO
    }
}
