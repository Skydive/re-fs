//! Positional file I/O (pread/pwrite style) on every platform.

use std::fs::File;
use std::io;

#[cfg(unix)]
pub fn read_exact_at(f: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(f, buf, offset)
}

#[cfg(unix)]
pub fn read_at(f: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(f, buf, offset)
}

#[cfg(unix)]
pub fn write_all_at(f: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(f, buf, offset)
}

// Windows: seek_read/seek_write move the file pointer, which is fine here: every
// access to these files names its offset.
#[cfg(windows)]
pub fn read_at(f: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(f, buf, offset)
}

#[cfg(windows)]
pub fn read_exact_at(f: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        match read_at(f, buf, offset)? {
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
pub fn write_all_at(f: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        match std::os::windows::fs::FileExt::seek_write(f, buf, offset)? {
            0 => return Err(io::ErrorKind::WriteZero.into()),
            n => {
                buf = &buf[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn round_trip() {
        let p = std::env::temp_dir().join(format!("re-fs-pio-{}", std::process::id()));
        let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&p).unwrap();
        super::write_all_at(&f, b"world", 6).unwrap();
        super::write_all_at(&f, b"hello ", 0).unwrap();
        let mut buf = [0u8; 5];
        super::read_exact_at(&f, &mut buf, 6).unwrap();
        assert_eq!(&buf, b"world");
        assert!(super::read_exact_at(&f, &mut [0u8; 4], 9).is_err(), "past the end");
        std::fs::remove_file(p).unwrap();
    }
}
