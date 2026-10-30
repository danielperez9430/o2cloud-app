//! How far the CLI has got through a file it is uploading. The CLI reports
//! no progress, but it streams the file from disk as it sends it, so the
//! read offset of its open file follows the upload (ahead only by the
//! socket buffers).
//!
//! macOS and Linux expose the offset of the CLI's open file. Windows does not
//! make another process's file handles readable without a debugger, so there
//! we use the process's total read bytes instead. Uploads run one at a time,
//! so that is close enough for a percentage and, more importantly, lets the
//! watchdog notice a stalled upload.

use std::path::Path;

/// A local file, identified the way another process's open files can be
/// matched against it.
#[cfg(unix)]
pub struct FileId {
    dev: u64,
    ino: u64,
}

/// On Windows the whole process's read counter is used, so the file's size
/// is all that is needed (to keep the estimate from going past 100%).
#[cfg(windows)]
pub struct FileId {
    size: u64,
}

impl FileId {
    pub fn of(path: &Path) -> Option<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let meta = std::fs::metadata(path).ok()?;
            Some(Self {
                dev: meta.dev(),
                ino: meta.ino(),
            })
        }
        #[cfg(windows)]
        {
            let meta = std::fs::metadata(path).ok()?;
            if !meta.is_file() {
                return None;
            }
            Some(Self { size: meta.len() })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            None
        }
    }
}

/// The read offset of `file` in process `pid`, if it has it open.
#[cfg(target_os = "macos")]
pub fn read_offset(pid: u32, file: &FileId) -> Option<u64> {
    use std::mem::{MaybeUninit, size_of};

    // From sys/proc_info.h; libc has `vnode_info` but not these.
    #[repr(C)]
    struct ProcFileInfo {
        fi_openflags: u32,
        fi_status: u32,
        fi_offset: libc::off_t,
        fi_type: i32,
        fi_guardflags: u32,
    }
    #[repr(C)]
    struct VnodeFdInfo {
        pfi: ProcFileInfo,
        pvi: libc::vnode_info,
    }
    const PROC_PIDFDVNODEINFO: libc::c_int = 1;

    let pid = libc::c_int::try_from(pid).ok()?;
    // SAFETY: a null buffer asks for the size needed.
    let needed =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return None;
    }
    // Room for a few descriptors opened in between.
    let mut fds: Vec<libc::proc_fdinfo> =
        Vec::with_capacity(needed as usize / size_of::<libc::proc_fdinfo>() + 8);
    // SAFETY: the buffer is valid for `capacity` entries; the call returns
    // how many bytes it filled.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr().cast(),
            (fds.capacity() * size_of::<libc::proc_fdinfo>()) as libc::c_int,
        )
    };
    if written <= 0 {
        return None;
    }
    // SAFETY: the kernel initialised `written` bytes of whole entries.
    unsafe { fds.set_len(written as usize / size_of::<libc::proc_fdinfo>()) };

    for fd in fds
        .iter()
        .filter(|fd| fd.proc_fdtype == libc::PROX_FDTYPE_VNODE as u32)
    {
        let mut info = MaybeUninit::<VnodeFdInfo>::zeroed();
        let size = size_of::<VnodeFdInfo>() as libc::c_int;
        // SAFETY: `info` is a buffer of `size` bytes.
        let got = unsafe {
            libc::proc_pidfdinfo(
                pid,
                fd.proc_fd,
                PROC_PIDFDVNODEINFO,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if got != size {
            continue;
        }
        // SAFETY: the kernel filled the whole struct (checked above).
        let info = unsafe { info.assume_init() };
        let stat = &info.pvi.vi_stat;
        if u64::from(stat.vst_dev) == file.dev && stat.vst_ino == file.ino {
            return u64::try_from(info.pfi.fi_offset).ok();
        }
    }
    None
}

/// The read offset of `file` in process `pid`, if it has it open.
#[cfg(target_os = "linux")]
pub fn read_offset(pid: u32, file: &FileId) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    for entry in std::fs::read_dir(format!("/proc/{pid}/fd")).ok()?.flatten() {
        // `metadata` follows the /proc link to the open file itself.
        let Ok(meta) = std::fs::metadata(entry.path()) else {
            continue;
        };
        if meta.dev() != file.dev || meta.ino() != file.ino {
            continue;
        }
        let fdinfo = std::fs::read_to_string(format!(
            "/proc/{pid}/fdinfo/{}",
            entry.file_name().to_string_lossy()
        ))
        .ok()?;
        return fdinfo
            .lines()
            .find_map(|line| line.strip_prefix("pos:"))
            .and_then(|pos| pos.trim().parse().ok());
    }
    None
}

/// Windows does not expose another process's file handles to a normal
/// process. Its total read bytes are a good proxy while one upload runs, and
/// they are clamped to the file's size.
#[cfg(windows)]
pub fn read_offset(pid: u32, file: &FileId) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Threading::{
        GetProcessIoCounters, IO_COUNTERS, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: the handle is closed below; the counter buffer is local and
    // valid for the call.
    unsafe {
        let handle: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut counters: IO_COUNTERS = std::mem::zeroed();
        let ok = GetProcessIoCounters(handle, &mut counters);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        Some(counters.ReadTransferCount.min(file.size))
    }
}

/// Elsewhere the upload shows as in progress, without a percentage.
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub fn read_offset(_pid: u32, _file: &FileId) -> Option<u64> {
    None
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn reads_the_offset_of_a_file_this_process_has_open() {
        let path = std::env::temp_dir().join(format!("o2-progress-{}", std::process::id()));
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&[0u8; 10_000])
            .unwrap();

        let mut reader = std::fs::File::open(&path).unwrap();
        let mut buffer = [0u8; 1234];
        reader.read_exact(&mut buffer).unwrap();

        let id = FileId::of(&path).unwrap();
        assert_eq!(read_offset(std::process::id(), &id), Some(1234));

        drop(reader);
        assert_eq!(read_offset(std::process::id(), &id), None);
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn file_ids_keep_the_file_size_and_offsets_stay_within_it() {
        let path = std::env::temp_dir().join(format!("o2-progress-{}.bin", std::process::id()));
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&[0u8; 10_000])
            .unwrap();

        let mut reader = std::fs::File::open(&path).unwrap();
        let mut buffer = [0u8; 1234];
        reader.read_exact(&mut buffer).unwrap();

        let id = FileId::of(&path).unwrap();
        assert_eq!(id.size, 10_000);
        let offset = read_offset(std::process::id(), &id);
        assert!(offset.is_some());
        assert!(offset.unwrap() <= 10_000);

        drop(reader);
        std::fs::remove_file(path).unwrap();
    }
}
