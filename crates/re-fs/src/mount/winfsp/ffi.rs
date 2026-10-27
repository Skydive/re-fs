//! The parts of WinFsp's native API (winfsp/winfsp.h, winfsp/fsctl.h, v2.x) re-fs
//! uses. winfsp-x64.dll is loaded at run time, so the build needs no WinFsp SDK and a
//! machine without WinFsp gets a clear error instead of failing to start.

// Types keep their C names (NTSTATUS, PWSTR) so they read like the headers.
#![allow(clippy::upper_case_acronyms)]

use std::ffi::c_void;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};

pub type NTSTATUS = i32;
pub type PWSTR = *mut u16;
pub type FileSystem = c_void;

pub const STATUS_SUCCESS: NTSTATUS = 0;
pub const STATUS_BUFFER_OVERFLOW: NTSTATUS = 0x8000_0005_u32 as i32;
pub const STATUS_INVALID_HANDLE: NTSTATUS = 0xC000_0008_u32 as i32;
pub const STATUS_INVALID_PARAMETER: NTSTATUS = 0xC000_000D_u32 as i32;
pub const STATUS_INVALID_DEVICE_REQUEST: NTSTATUS = 0xC000_0010_u32 as i32;
pub const STATUS_END_OF_FILE: NTSTATUS = 0xC000_0011_u32 as i32;
pub const STATUS_ACCESS_DENIED: NTSTATUS = 0xC000_0022_u32 as i32;
pub const STATUS_OBJECT_NAME_NOT_FOUND: NTSTATUS = 0xC000_0034_u32 as i32;
pub const STATUS_OBJECT_NAME_COLLISION: NTSTATUS = 0xC000_0035_u32 as i32;
pub const STATUS_OBJECT_PATH_NOT_FOUND: NTSTATUS = 0xC000_003A_u32 as i32;
pub const STATUS_MEDIA_WRITE_PROTECTED: NTSTATUS = 0xC000_00A2_u32 as i32;
pub const STATUS_IO_TIMEOUT: NTSTATUS = 0xC000_00B5_u32 as i32;
pub const STATUS_FILE_IS_A_DIRECTORY: NTSTATUS = 0xC000_00BA_u32 as i32;
pub const STATUS_UNEXPECTED_IO_ERROR: NTSTATUS = 0xC000_00E9_u32 as i32;
pub const STATUS_DIRECTORY_NOT_EMPTY: NTSTATUS = 0xC000_0101_u32 as i32;
pub const STATUS_NOT_A_DIRECTORY: NTSTATUS = 0xC000_0103_u32 as i32;
pub const STATUS_RETRY: NTSTATUS = 0xC000_022D_u32 as i32;
pub const STATUS_HOST_UNREACHABLE: NTSTATUS = 0xC000_023D_u32 as i32;

pub const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
pub const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
pub const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;
pub const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
/// CreateOptions bit: the caller wants a directory.
pub const FILE_DIRECTORY_FILE: u32 = 0x1;
/// GrantedAccess bit: the caller will read the file's data.
pub const FILE_READ_DATA: u32 = 0x1;

pub const FILE_NOTIFY_CHANGE_FILE_NAME: u32 = 0x1;
pub const FILE_NOTIFY_CHANGE_DIR_NAME: u32 = 0x2;
pub const FILE_NOTIFY_CHANGE_ATTRIBUTES: u32 = 0x4;
pub const FILE_NOTIFY_CHANGE_SIZE: u32 = 0x8;
pub const FILE_NOTIFY_CHANGE_LAST_WRITE: u32 = 0x10;
pub const FILE_ACTION_ADDED: u32 = 1;
pub const FILE_ACTION_REMOVED: u32 = 2;
pub const FILE_ACTION_MODIFIED: u32 = 3;

/// Cleanup flag: the file is to be deleted (after CanDelete allowed it).
pub const FSP_CLEANUP_DELETE: u32 = 0x1;

/// FSP_FSCTL_VOLUME_PARAMS. The C bitfields are one `u32` each, lowest bit first.
#[repr(C)]
pub struct VolumeParams {
    pub version: u16,
    pub sector_size: u16,
    pub sectors_per_allocation_unit: u16,
    pub max_component_length: u16,
    pub volume_creation_time: u64,
    pub volume_serial_number: u32,
    pub transact_timeout: u32,
    pub irp_timeout: u32,
    pub irp_capacity: u32,
    pub file_info_timeout: u32,
    pub flags: u32,
    pub prefix: [u16; 192],
    pub file_system_name: [u16; 16],
    pub flags_v1: u32,
    pub volume_info_timeout: u32,
    pub dir_info_timeout: u32,
    pub security_timeout: u32,
    pub stream_info_timeout: u32,
    pub ea_timeout: u32,
    pub fsext_control_code: u32,
    pub reserved32: [u32; 1],
    pub reserved64: [u64; 2],
}
const _: () = assert!(std::mem::size_of::<VolumeParams>() == 504);

/// Bits of `VolumeParams::flags`.
pub mod vp {
    pub const CASE_PRESERVED_NAMES: u32 = 1 << 1;
    pub const UNICODE_ON_DISK: u32 = 1 << 2;
    pub const PERSISTENT_ACLS: u32 = 1 << 3;
    pub const READ_ONLY_VOLUME: u32 = 1 << 9;
    pub const POST_CLEANUP_WHEN_MODIFIED_ONLY: u32 = 1 << 10;
    pub const UM_FILE_CONTEXT_IS_USER_CONTEXT2: u32 = 1 << 16;
}

/// FSP_FSCTL_VOLUME_INFO
#[repr(C)]
pub struct VolumeInfo {
    pub total_size: u64,
    pub free_size: u64,
    pub volume_label_length: u16,
    pub volume_label: [u16; 32],
}
const _: () = assert!(std::mem::size_of::<VolumeInfo>() == 88);

/// FSP_FSCTL_FILE_INFO
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FileInfo {
    pub file_attributes: u32,
    pub reparse_tag: u32,
    pub allocation_size: u64,
    pub file_size: u64,
    pub creation_time: u64,
    pub last_access_time: u64,
    pub last_write_time: u64,
    pub change_time: u64,
    pub index_number: u64,
    pub hard_links: u32,
    pub ea_size: u32,
}
const _: () = assert!(std::mem::size_of::<FileInfo>() == 72);

/// FSP_FSCTL_DIR_INFO, without its trailing name.
#[repr(C)]
pub struct DirInfo {
    pub size: u16,
    pub file_info: FileInfo,
    pub padding: [u64; 3],
}
pub const DIR_INFO_SIZE: usize = 104;
const _: () = assert!(std::mem::size_of::<DirInfo>() == DIR_INFO_SIZE);

/// FSP_FSCTL_NOTIFY_INFO, without its trailing name.
#[repr(C)]
pub struct NotifyInfo {
    pub size: u16,
    pub filter: u32,
    pub action: u32,
}
pub const NOTIFY_INFO_SIZE: usize = 12;
const _: () = assert!(std::mem::size_of::<NotifyInfo>() == NOTIFY_INFO_SIZE);

/// The start of FSP_FILE_SYSTEM: just enough to reach UserContext.
#[repr(C)]
pub struct FileSystemHead {
    pub version: u16,
    pub user_context: *mut c_void,
}

type Sd = *mut c_void;

/// FSP_FILE_SYSTEM_INTERFACE: 64 pointer slots, of which re-fs fills those named.
#[repr(C)]
pub struct Interface {
    pub get_volume_info: Option<unsafe extern "system" fn(*mut FileSystem, *mut VolumeInfo) -> NTSTATUS>,
    pub set_volume_label: usize,
    pub get_security_by_name: Option<unsafe extern "system" fn(*mut FileSystem, PWSTR, *mut u32, Sd, *mut usize) -> NTSTATUS>,
    pub create: Option<unsafe extern "system" fn(*mut FileSystem, PWSTR, u32, u32, u32, Sd, u64, *mut *mut c_void, *mut FileInfo) -> NTSTATUS>,
    pub open: Option<unsafe extern "system" fn(*mut FileSystem, PWSTR, u32, u32, *mut *mut c_void, *mut FileInfo) -> NTSTATUS>,
    pub overwrite: Option<unsafe extern "system" fn(*mut FileSystem, *mut c_void, u32, u8, u64, *mut FileInfo) -> NTSTATUS>,
    pub cleanup: Option<unsafe extern "system" fn(*mut FileSystem, *mut c_void, PWSTR, u32)>,
    pub close: Option<unsafe extern "system" fn(*mut FileSystem, *mut c_void)>,
    pub read: Option<unsafe extern "system" fn(*mut FileSystem, *mut c_void, *mut c_void, u64, u32, *mut u32) -> NTSTATUS>,
    pub write: Option<unsafe extern "system" fn(*mut FileSystem, *mut c_void, *mut c_void, u64, u32, u8, u8, *mut u32, *mut FileInfo) -> NTSTATUS>,
    pub flush: Option<unsafe extern "system" fn(*mut FileSystem, *mut c_void, *mut FileInfo) -> NTSTATUS>,
    pub get_file_info: Option<unsafe extern "system" fn(*mut FileSystem, *mut c_void, *mut FileInfo) -> NTSTATUS>,
    pub set_basic_info: Option<unsafe extern "system" fn(*mut FileSystem, *mut c_void, u32, u64, u64, u64, u64, *mut FileInfo) -> NTSTATUS>,
    pub set_file_size: Option<unsafe extern "system" fn(*mut FileSystem, *mut c_void, u64, u8, *mut FileInfo) -> NTSTATUS>,
    pub can_delete: Option<unsafe extern "system" fn(*mut FileSystem, *mut c_void, PWSTR) -> NTSTATUS>,
    pub rename: Option<unsafe extern "system" fn(*mut FileSystem, *mut c_void, PWSTR, PWSTR, u8) -> NTSTATUS>,
    pub get_security: Option<unsafe extern "system" fn(*mut FileSystem, *mut c_void, Sd, *mut usize) -> NTSTATUS>,
    pub set_security: Option<unsafe extern "system" fn(*mut FileSystem, *mut c_void, u32, Sd) -> NTSTATUS>,
    pub read_directory: Option<unsafe extern "system" fn(*mut FileSystem, *mut c_void, PWSTR, PWSTR, *mut c_void, u32, *mut u32) -> NTSTATUS>,
    /// ResolveReparsePoints … DispatcherStopped (14 slots), then Reserved[31].
    pub unused: [usize; 14 + 31],
}
const _: () = assert!(std::mem::size_of::<Interface>() == 64 * 8);

/// The DLL's functions.
pub struct Api {
    pub create: unsafe extern "system" fn(PWSTR, *const VolumeParams, *const Interface, *mut *mut FileSystem) -> NTSTATUS,
    pub delete: unsafe extern "system" fn(*mut FileSystem),
    pub set_mount_point: unsafe extern "system" fn(*mut FileSystem, PWSTR) -> NTSTATUS,
    pub start_dispatcher: unsafe extern "system" fn(*mut FileSystem, u32) -> NTSTATUS,
    pub stop_dispatcher: unsafe extern "system" fn(*mut FileSystem),
    pub add_dir_info: unsafe extern "system" fn(*mut DirInfo, *mut c_void, u32, *mut u32) -> u8,
    pub notify_begin: unsafe extern "system" fn(*mut FileSystem, u32) -> NTSTATUS,
    pub notify_end: unsafe extern "system" fn(*mut FileSystem) -> NTSTATUS,
    pub notify: unsafe extern "system" fn(*mut FileSystem, *mut NotifyInfo, usize) -> NTSTATUS,
    pub add_notify_info: unsafe extern "system" fn(*mut NotifyInfo, *mut c_void, u32, *mut u32) -> u8,
    pub mount_point: unsafe extern "system" fn(*mut FileSystem) -> PWSTR,
    pub nt_status_from_win32: unsafe extern "system" fn(u32) -> NTSTATUS,
}

/// winfsp-x64.dll, loaded once: from WinFsp's install directory (as its own
/// FspLoad does), else from the DLL search path.
pub fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(load).as_ref().map_err(Clone::clone)
}

fn load() -> Result<Api, String> {
    let mut candidates = Vec::new();
    if let Some(dir) = install_dir() {
        candidates.push(format!("{}\\bin\\winfsp-x64.dll", dir.trim_end_matches('\\')));
    }
    candidates.push("winfsp-x64.dll".into());
    let module = candidates.iter().map(|p| unsafe { LoadLibraryW(wide(p).as_ptr()) }).find(|m| !m.is_null()).ok_or_else(|| {
        "WinFsp isn't installed (winfsp-x64.dll not found). Install it from https://winfsp.dev/rel/ and try again".to_string()
    })?;
    // SAFETY: each symbol is cast to the signature winfsp.h declares for it (the
    // field's type).
    #[allow(clippy::missing_transmute_annotations)]
    unsafe {
        Ok(Api {
            create: std::mem::transmute(sym(module, "FspFileSystemCreate")?),
            delete: std::mem::transmute(sym(module, "FspFileSystemDelete")?),
            set_mount_point: std::mem::transmute(sym(module, "FspFileSystemSetMountPoint")?),
            start_dispatcher: std::mem::transmute(sym(module, "FspFileSystemStartDispatcher")?),
            stop_dispatcher: std::mem::transmute(sym(module, "FspFileSystemStopDispatcher")?),
            add_dir_info: std::mem::transmute(sym(module, "FspFileSystemAddDirInfo")?),
            notify_begin: std::mem::transmute(sym(module, "FspFileSystemNotifyBegin")?),
            notify_end: std::mem::transmute(sym(module, "FspFileSystemNotifyEnd")?),
            notify: std::mem::transmute(sym(module, "FspFileSystemNotify")?),
            add_notify_info: std::mem::transmute(sym(module, "FspFileSystemAddNotifyInfo")?),
            mount_point: std::mem::transmute(sym(module, "FspFileSystemMountPointF")?),
            nt_status_from_win32: std::mem::transmute(sym(module, "FspNtStatusFromWin32")?),
        })
    }
}

unsafe fn sym(module: HMODULE, name: &str) -> Result<unsafe extern "system" fn() -> isize, String> {
    let c = std::ffi::CString::new(name).unwrap();
    GetProcAddress(module, c.as_ptr().cast()).ok_or_else(|| format!("winfsp-x64.dll has no {name}; WinFsp 2.0 or newer is needed"))
}

/// HKLM\SOFTWARE\WOW6432Node\WinFsp\InstallDir (WinFsp writes its key as a 32-bit app).
fn install_dir() -> Option<String> {
    let mut buf = [0u16; 520];
    let mut len = (buf.len() * 2) as u32;
    let key = wide("SOFTWARE\\WOW6432Node\\WinFsp");
    let value = wide("InstallDir");
    // SAFETY: buf and len describe a writable buffer of len bytes.
    let r = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
    (r == 0).then(|| from_wide(&buf[..(len as usize / 2)]))
}

/// A NUL-terminated UTF-16 copy of `s`.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// UTF-16 up to the first NUL, as a String.
pub fn from_wide(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

/// A NUL-terminated wide string from WinFsp (null gives "").
///
/// # Safety
/// `p` is null or points to a NUL-terminated UTF-16 string.
pub unsafe fn from_pwstr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    while *p.add(n) != 0 {
        n += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
}
