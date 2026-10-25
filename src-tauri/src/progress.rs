//! How far the CLI has got through a file it is uploading. The CLI reports
//! no progress, but it streams the file from disk as it sends it, so the
//! read offset of its open file follows the upload (ahead only by the
//! socket buffers).

use std::path::Path;

/// A local file, identified the way another process's open files can be
/// matched against it.
#[cfg_attr(not(unix), allow(dead_code))]
pub struct FileId {
    dev: u64,
    ino: u64,
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
        #[cfg(not(unix))]
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

/// Elsewhere the upload shows as in progress, without a percentage.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
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
