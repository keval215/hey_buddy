//! Windows installer staging and launch (ADR 0005 section 7). UNTESTED on this machine: compiled
//! for `x86_64-pc-windows-gnu` only. The only `CreateProcessW` call of the update crate.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::marker::PhantomData;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use ring::rand::SecureRandom;
use sha2::{Digest, Sha256};
use windows::core::{HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, LocalFree, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::CreateDirectoryW;
use windows::Win32::System::Threading::{
    CreateProcessW, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW,
};

use super::{
    any_link_in_chain, command_line, run_dir_name, setup_path, LaunchError, PRIVATE_DIR_SDDL,
};
use crate::Artifact;

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_SHARE_READ: u32 = 1;
const FILE_SHARE_WRITE: u32 = 2;
const FILE_LIST_DIRECTORY: u32 = 1;
const ERROR_ELEVATION_REQUIRED: u32 = 740;

/// A hashed copy of the installer in a private directory. `!Send`; holds the file handle (no
/// writers, no delete / rename) and a directory handle (no rename of the directory) until
/// [`StagedInstaller::launch`] has started the process or the value is dropped. Only [`stage`]
/// constructs it.
pub struct StagedInstaller {
    _file: File,
    _dir: File,
    exe: PathBuf,
    dir: PathBuf,
    _not_send: PhantomData<*const ()>,
}

fn is_link(p: &Path) -> Result<bool, LaunchError> {
    match std::fs::symlink_metadata(p) {
        Ok(md) => {
            Ok(md.file_type().is_symlink()
                || md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(LaunchError::Io),
    }
}

fn wide(p: &std::ffi::OsStr) -> Vec<u16> {
    p.encode_wide().chain(Some(0)).collect()
}

/// `CreateDirectoryW` with the protected DACL. Fails if the directory already exists.
fn create_private_dir(dir: &Path) -> Result<(), LaunchError> {
    let sddl: Vec<u16> = PRIVATE_DIR_SDDL.encode_utf16().chain(Some(0)).collect();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `sddl` is NUL-terminated; `sd` receives a LocalAlloc'd descriptor freed below.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
    }
    .map_err(|_| LaunchError::Io)?;
    let attrs = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: false.into(),
    };
    let path = wide(dir.as_os_str());
    // SAFETY: `path` is NUL-terminated and `attrs` points at a valid descriptor for the call.
    let created = unsafe { CreateDirectoryW(PCWSTR(path.as_ptr()), Some(&attrs)) };
    // SAFETY: `sd.0` came from the call above and is not used after this.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(sd.0)));
    }
    created.map_err(|_| LaunchError::Io)
}

/// Copy `downloaded` into a fresh private directory below `run_root` as `setup.exe`, then hash
/// the COPY through a handle that denies writers and delete / rename, and compare size and
/// sha256 with the verified `artifact`. The handles stay open inside the returned value.
pub fn stage(
    downloaded: &Path,
    run_root: &Path,
    artifact: &Artifact,
) -> Result<StagedInstaller, LaunchError> {
    // Neither run_root nor any ancestor up to the drive root may be a link / reparse point.
    if any_link_in_chain(run_root, is_link)? {
        return Err(LaunchError::UnsafePath);
    }
    std::fs::create_dir_all(run_root).map_err(|_| LaunchError::Io)?;
    if is_link(run_root)? {
        return Err(LaunchError::UnsafePath);
    }
    let mut random = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| LaunchError::Io)?;
    let dir = run_root.join(run_dir_name(&random));
    create_private_dir(&dir)?;
    match copy_and_hash(downloaded, &dir, artifact) {
        Ok(s) => Ok(s),
        Err(e) => {
            // Every handle was dropped on the way out, so the directory can go.
            let _ = std::fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

fn copy_and_hash(
    downloaded: &Path,
    dir: &Path,
    artifact: &Artifact,
) -> Result<StagedInstaller, LaunchError> {
    if is_link(downloaded)? {
        return Err(LaunchError::UnsafePath);
    }
    let exe = setup_path(dir);
    {
        let src = File::open(downloaded).map_err(|_| LaunchError::Io)?;
        let mut dst = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&exe)
            .map_err(|_| LaunchError::Io)?;
        // One byte more than the verified size: a longer file is caught below.
        let copied = std::io::copy(&mut src.take(artifact.size().saturating_add(1)), &mut dst)
            .map_err(|_| LaunchError::Io)?;
        if copied != artifact.size() {
            return Err(LaunchError::HashMismatch);
        }
        dst.sync_all().map_err(|_| LaunchError::Io)?;
    }
    // Readers only: nobody can write, delete or rename the file while these handles live.
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&exe)
        .map_err(|_| LaunchError::Io)?;
    // No FILE_SHARE_DELETE: the directory cannot be renamed or removed under us.
    let dir_handle = OpenOptions::new()
        .access_mode(FILE_LIST_DIRECTORY)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(dir)
        .map_err(|_| LaunchError::Io)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut len = 0u64;
    let mut reader = &file;
    loop {
        let n = reader.read(&mut buf).map_err(|_| LaunchError::Io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        len += n as u64;
    }
    if len != artifact.size() || hasher.finalize().as_slice() != artifact.sha256() {
        return Err(LaunchError::HashMismatch);
    }
    Ok(StagedInstaller {
        _file: file,
        _dir: dir_handle,
        exe,
        dir: dir.to_path_buf(),
        _not_send: PhantomData,
    })
}

impl StagedInstaller {
    /// `CreateProcessW(lpApplicationName = the full path, lpCommandLine = "\"<path>\"" only)`.
    /// No arguments, no shell, no inherited handles, no environment override. The handles taken
    /// by `stage` are released when this returns.
    pub fn launch(self) -> Result<(), LaunchError> {
        let exe_w = wide(self.exe.as_os_str());
        let cmd = command_line(self.exe.to_str().ok_or(LaunchError::UnsafePath)?)?;
        let mut cmd_w: Vec<u16> = cmd.encode_utf16().chain(Some(0)).collect();
        let cwd_w = wide(self.dir.as_os_str());
        let si = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: every pointer is valid and NUL-terminated for the duration of the call;
        // `cmd_w` is a mutable buffer as CreateProcessW requires.
        let res = unsafe {
            CreateProcessW(
                PCWSTR(exe_w.as_ptr()),
                Some(PWSTR(cmd_w.as_mut_ptr())),
                None,
                None,
                false,
                PROCESS_CREATION_FLAGS(0),
                None,
                PCWSTR(cwd_w.as_ptr()),
                &si,
                &mut pi,
            )
        };
        match res {
            Ok(()) => {
                // SAFETY: both handles were just returned by CreateProcessW.
                unsafe {
                    let _ = CloseHandle(pi.hProcess);
                    let _ = CloseHandle(pi.hThread);
                }
                Ok(())
            }
            Err(e) if e.code() == HRESULT::from_win32(ERROR_ELEVATION_REQUIRED) => {
                Err(LaunchError::NeedsAdmin)
            }
            Err(_) => Err(LaunchError::Blocked),
        }
    }
}
