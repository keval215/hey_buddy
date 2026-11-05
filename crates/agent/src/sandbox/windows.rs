//! `WindowsSandbox` (ADR 0003 section 3). UNTESTED on this machine: compile-checked for
//! `x86_64-pc-windows-gnu` only. Every decision that can be pure (path validation, command line,
//! environment block, capability list, job limits) lives in `spec.rs` and is unit-tested on Linux;
//! this file only feeds those values into Win32 calls. Windows behaviour to verify is listed in
//! docs/manual-tests.md, "Agent engine sandbox (S2-10/15)".
//!
//! Launch, in this order. ANY failure means NOTHING is spawned (or a suspended child is
//! terminated before it ever ran) and the result is `SandboxError::Failed(<step>)`: there is no
//! unsandboxed fallback.
//!  1. validate `<install_dir>\buddy-agent.exe` (absolute local path, nothing else);
//!  2. AppContainer profile `hey_buddy.agent` with ZERO capabilities (no network incl. loopback,
//!     no user files); its SID and folder;
//!  3. minimal environment block (SystemRoot, TEMP, TMP = container folder), working dir = the
//!     container folder;
//!  4. three anonymous pipes, the parent's ends non-inheritable;
//!  5. attribute list: SECURITY_CAPABILITIES (the container SID), HANDLE_LIST (exactly the three
//!     child pipe ends), CHILD_PROCESS_POLICY (restricted), MITIGATION_POLICY;
//!  6. Job Object: KILL_ON_JOB_CLOSE, DIE_ON_UNHANDLED_EXCEPTION, ACTIVE_PROCESS = 1, 512 MiB,
//!     UI restrictions;
//!  7. `CreateProcessW(EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_NO_WINDOW |
//!     CREATE_UNICODE_ENVIRONMENT, bInheritHandles = TRUE)`, assign to the job, `ResumeThread`.
//!
//! Kill = `TerminateJobObject` (non-blocking, idempotent). When the last process of a spawn is
//! gone the AppContainer profile (and with it the container folder) is deleted.

use std::fs::File;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::sync::{Arc, Mutex};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    SetHandleInformation, HANDLE, HANDLE_FLAGS, HANDLE_FLAG_INHERIT, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
    DeriveAppContainerSidFromAppContainerName, GetAppContainerFolderPath,
};
use windows::Win32::Security::{FreeSid, PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicUIRestrictions,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
    JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT,
    JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_UILIMIT,
    JOB_OBJECT_UILIMIT_DESKTOP, JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS,
    JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES, JOB_OBJECT_UILIMIT_READCLIPBOARD,
    JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
};
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList, ResumeThread,
    TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject, CREATE_NO_WINDOW,
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    STARTUPINFOW,
};

use super::spec;
use super::{EngineProcess, EngineSandbox, KillHandle, LaunchSpec, ProcessStreams, SandboxError};

// The pure constants in `spec` must equal the real ones (checked at compile time).
const _: () = {
    assert!(spec::LIMIT_ACTIVE_PROCESS == JOB_OBJECT_LIMIT_ACTIVE_PROCESS.0);
    assert!(spec::LIMIT_PROCESS_MEMORY == JOB_OBJECT_LIMIT_PROCESS_MEMORY.0);
    assert!(
        spec::LIMIT_DIE_ON_UNHANDLED_EXCEPTION == JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION.0
    );
    assert!(spec::LIMIT_KILL_ON_JOB_CLOSE == JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE.0);
    assert!(spec::UILIMIT_HANDLES == JOB_OBJECT_UILIMIT_HANDLES.0);
    assert!(spec::UILIMIT_READCLIPBOARD == JOB_OBJECT_UILIMIT_READCLIPBOARD.0);
    assert!(spec::UILIMIT_WRITECLIPBOARD == JOB_OBJECT_UILIMIT_WRITECLIPBOARD.0);
    assert!(spec::UILIMIT_SYSTEMPARAMETERS == JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS.0);
    assert!(spec::UILIMIT_DISPLAYSETTINGS == JOB_OBJECT_UILIMIT_DISPLAYSETTINGS.0);
    assert!(spec::UILIMIT_GLOBALATOMS == JOB_OBJECT_UILIMIT_GLOBALATOMS.0);
    assert!(spec::UILIMIT_DESKTOP == JOB_OBJECT_UILIMIT_DESKTOP.0);
    assert!(spec::UILIMIT_EXITWINDOWS == JOB_OBJECT_UILIMIT_EXITWINDOWS.0);
};

/// `HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)`.
const HR_ALREADY_EXISTS: i32 = 0x8007_00B7_u32 as i32;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn h(o: &OwnedHandle) -> HANDLE {
    HANDLE(o.as_raw_handle())
}

fn owned(handle: HANDLE) -> OwnedHandle {
    // SAFETY: `handle` was just returned by the OS to us and is owned exactly once from here on.
    unsafe { OwnedHandle::from_raw_handle(handle.0 as RawHandle) }
}

/// Number of live spawns of this process; the profile is deleted when it drops to zero. One lock
/// serialises create and delete so a new spawn can never lose its profile to an old reaper.
static LIVE: Mutex<usize> = Mutex::new(0);

/// One counted reference to the profile. Acquired before the profile is created, released exactly
/// once on drop: on a failed spawn, or when the `WinProcess` that owns it is dropped or reaped
/// (also if `wait` is never called).
struct ProfileRef(());

impl ProfileRef {
    fn acquire() -> Self {
        *lock(&LIVE) += 1;
        ProfileRef(())
    }
}

impl Drop for ProfileRef {
    fn drop(&mut self) {
        release_profile();
    }
}

fn release_profile() {
    let mut n = lock(&LIVE);
    *n = n.saturating_sub(1);
    if *n == 0 {
        let name = wide(spec::APPCONTAINER_NAME);
        // SAFETY: NUL-terminated name; failure is harmless (the folder is empty and unwritable by others).
        unsafe {
            let _ = DeleteAppContainerProfile(PCWSTR(name.as_ptr()));
        }
    }
}

pub struct WindowsSandbox {
    _private: (),
}

impl WindowsSandbox {
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl Default for WindowsSandbox {
    fn default() -> Self {
        Self::new()
    }
}

/// Owns the container SID.
struct Sid(PSID);
impl Drop for Sid {
    fn drop(&mut self) {
        // SAFETY: the SID came from CreateAppContainerProfile / DeriveAppContainerSid... and is
        // freed exactly once.
        unsafe {
            let _ = FreeSid(self.0);
        }
    }
}

fn fail(step: &'static str) -> SandboxError {
    SandboxError::Failed(step)
}

fn container_sid() -> Result<Sid, SandboxError> {
    let name = wide(spec::APPCONTAINER_NAME);
    let display = wide(spec::APPCONTAINER_DISPLAY);
    // SAFETY: NUL-terminated strings that outlive the calls; ZERO capabilities (`None`).
    unsafe {
        match CreateAppContainerProfile(
            PCWSTR(name.as_ptr()),
            PCWSTR(display.as_ptr()),
            PCWSTR(display.as_ptr()),
            None,
        ) {
            Ok(sid) => Ok(Sid(sid)),
            Err(e) if e.code().0 == HR_ALREADY_EXISTS => {
                DeriveAppContainerSidFromAppContainerName(PCWSTR(name.as_ptr()))
                    .map(Sid)
                    .map_err(|_| fail("profile"))
            }
            Err(_) => Err(fail("profile")),
        }
    }
}

fn container_folder(sid: &Sid) -> Result<String, SandboxError> {
    let mut sid_str = PWSTR::null();
    // SAFETY: `sid.0` is a valid SID; the string it returns is freed with LocalFree below (the
    // `windows` crate declares LocalFree in Foundation; CoTaskMemFree frees the folder path).
    unsafe {
        ConvertSidToStringSidW(sid.0, &mut sid_str).map_err(|_| fail("sid_string"))?;
        let folder = GetAppContainerFolderPath(PCWSTR(sid_str.0));
        let _ = windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(
            sid_str.0 as *mut core::ffi::c_void,
        )));
        let folder = folder.map_err(|_| fail("folder"))?;
        let s = folder.to_string().map_err(|_| fail("folder"));
        CoTaskMemFree(Some(folder.0 as *const core::ffi::c_void));
        let s = s?;
        spec::validate_local_path(&s).map_err(|_| fail("folder"))?;
        Ok(s)
    }
}

struct Pipes {
    child_stdin: OwnedHandle,
    parent_stdin: OwnedHandle,
    parent_stdout: OwnedHandle,
    child_stdout: OwnedHandle,
    parent_stderr: OwnedHandle,
    child_stderr: OwnedHandle,
}

/// `(read, write)`; both inheritable, the caller clears the parent's end.
fn make_pipe() -> Result<(OwnedHandle, OwnedHandle), SandboxError> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: core::ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    let (mut r, mut w) = (HANDLE::default(), HANDLE::default());
    // SAFETY: valid out pointers and a valid SECURITY_ATTRIBUTES.
    unsafe {
        windows::Win32::System::Pipes::CreatePipe(&mut r, &mut w, Some(&sa), 0)
            .map_err(|_| fail("pipe"))?;
    }
    Ok((owned(r), owned(w)))
}

fn not_inheritable(o: &OwnedHandle) -> Result<(), SandboxError> {
    // SAFETY: valid handle owned by us.
    unsafe { SetHandleInformation(h(o), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }
        .map_err(|_| fail("pipe_flags"))
}

fn make_pipes() -> Result<Pipes, SandboxError> {
    let (child_stdin, parent_stdin) = make_pipe()?;
    let (parent_stdout, child_stdout) = make_pipe()?;
    let (parent_stderr, child_stderr) = make_pipe()?;
    not_inheritable(&parent_stdin)?;
    not_inheritable(&parent_stdout)?;
    not_inheritable(&parent_stderr)?;
    Ok(Pipes {
        child_stdin,
        parent_stdin,
        parent_stdout,
        child_stdout,
        parent_stderr,
        child_stderr,
    })
}

/// The process attribute list; deleted on drop.
struct AttrList {
    buf: Vec<usize>,
}

impl AttrList {
    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.buf.as_mut_ptr() as *mut core::ffi::c_void)
    }
}

impl Drop for AttrList {
    fn drop(&mut self) {
        // SAFETY: initialised by InitializeProcThreadAttributeList in `attr_list`.
        unsafe { DeleteProcThreadAttributeList(self.ptr()) };
    }
}

fn attr_list(count: u32) -> Result<AttrList, SandboxError> {
    let mut size = 0usize;
    // SAFETY: the first call only reports the needed size (it fails with ERROR_INSUFFICIENT_BUFFER).
    unsafe {
        let _ = InitializeProcThreadAttributeList(None, count, None, &mut size);
    }
    if size == 0 {
        return Err(fail("attr_size"));
    }
    let mut buf = vec![0usize; size.div_ceil(size_of::<usize>())];
    let ptr = LPPROC_THREAD_ATTRIBUTE_LIST(buf.as_mut_ptr() as *mut core::ffi::c_void);
    // SAFETY: `buf` is at least `size` bytes and pointer-aligned. On failure nothing was
    // initialised, so no `AttrList` (whose Drop deletes the list) is created.
    unsafe { InitializeProcThreadAttributeList(Some(ptr), count, None, &mut size) }
        .map_err(|_| fail("attr_init"))?;
    Ok(AttrList { buf })
}

/// Add one attribute. `value` must outlive the CreateProcessW call.
///
/// # Safety
/// `value` must point to `size` readable bytes that stay valid until the process is created.
unsafe fn set_attr(
    l: &mut AttrList,
    attr: u32,
    value: *const core::ffi::c_void,
    size: usize,
) -> Result<(), SandboxError> {
    UpdateProcThreadAttribute(l.ptr(), 0, attr as usize, Some(value), size, None, None)
        .map_err(|_| fail("attr_update"))
}

struct Job(OwnedHandle);

fn make_job() -> Result<Job, SandboxError> {
    // SAFETY: plain object creation and configuration with valid, correctly sized structs.
    unsafe {
        let job = owned(CreateJobObjectW(None, PCWSTR::null()).map_err(|_| fail("job"))?);
        let mut ext = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        ext.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT(spec::JOB_LIMIT_FLAGS);
        ext.BasicLimitInformation.ActiveProcessLimit = spec::JOB_ACTIVE_PROCESS_LIMIT;
        ext.ProcessMemoryLimit = spec::JOB_MEMORY_LIMIT_BYTES;
        SetInformationJobObject(
            h(&job),
            JobObjectExtendedLimitInformation,
            &ext as *const _ as *const core::ffi::c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .map_err(|_| fail("job_limits"))?;
        let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: JOB_OBJECT_UILIMIT(spec::JOB_UI_LIMITS),
        };
        SetInformationJobObject(
            h(&job),
            JobObjectBasicUIRestrictions,
            &ui as *const _ as *const core::ffi::c_void,
            size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
        )
        .map_err(|_| fail("job_ui"))?;
        Ok(Job(job))
    }
}

impl EngineSandbox for WindowsSandbox {
    fn spawn(&self, ls: &LaunchSpec) -> Result<Box<dyn EngineProcess>, SandboxError> {
        // 1. Path: only <install_dir>\buddy-agent.exe.
        let exe = ls.exe.to_str().ok_or(fail("path"))?;
        let dir = ls.install_dir.to_str().ok_or(fail("path"))?;
        spec::validate_exe_path(exe, dir).map_err(|_| fail("path"))?;
        let cmd = spec::quote_command_line(exe).ok_or(fail("path"))?;
        let system_root = std::env::var("SystemRoot").map_err(|_| fail("environment"))?;
        spec::validate_local_path(&system_root).map_err(|_| fail("environment"))?;

        // 2. Profile (counted so the last reaper deletes it).
        let profile = ProfileRef::acquire();
        let spawned = (move || {
            let guard = lock(&LIVE);
            let sid = container_sid();
            drop(guard);
            let sid = sid?;
            let folder = container_folder(&sid)?;
            // On any error the guard is dropped (released) on the way out.
            launch(exe, &cmd, &system_root, &folder, &sid, profile)
        })();
        spawned.map(|p| Box::new(p) as Box<dyn EngineProcess>)
    }
}

struct WinProcess {
    streams: Option<ProcessStreams>,
    process: OwnedHandle,
    job: Arc<Job>,
    /// Released when this process object is dropped (after `wait`, or without one).
    _profile: ProfileRef,
}

fn launch(
    exe: &str,
    cmd: &str,
    system_root: &str,
    folder: &str,
    sid: &Sid,
    profile: ProfileRef,
) -> Result<WinProcess, SandboxError> {
    // 3. Environment block and directory.
    let env = spec::build_env_block(system_root, folder).ok_or(fail("environment"))?;
    // 4. Pipes.
    let pipes = make_pipes()?;
    // 6. Job (before the process exists so a failure here spawns nothing).
    let job = make_job()?;

    // 5. Attributes. Every value below outlives CreateProcessW.
    let caps = SECURITY_CAPABILITIES {
        AppContainerSid: sid.0,
        Capabilities: core::ptr::null_mut(),
        CapabilityCount: spec::CAPABILITIES.len() as u32,
        Reserved: 0,
    };
    let inherit = [
        h(&pipes.child_stdin),
        h(&pipes.child_stdout),
        h(&pipes.child_stderr),
    ];
    let child_policy: u32 = spec::CHILD_PROCESS_RESTRICTED;
    let mitigation: u64 = spec::MITIGATION_POLICY;
    let mut attrs = attr_list(4)?;
    // SAFETY: each pointer is to a local that lives until after CreateProcessW.
    unsafe {
        set_attr(
            &mut attrs,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
            &caps as *const _ as *const core::ffi::c_void,
            size_of::<SECURITY_CAPABILITIES>(),
        )?;
        set_attr(
            &mut attrs,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            inherit.as_ptr() as *const core::ffi::c_void,
            size_of::<HANDLE>() * inherit.len(),
        )?;
        set_attr(
            &mut attrs,
            PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY,
            &child_policy as *const _ as *const core::ffi::c_void,
            size_of::<u32>(),
        )?;
        set_attr(
            &mut attrs,
            PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY,
            &mitigation as *const _ as *const core::ffi::c_void,
            size_of::<u64>(),
        )?;
    }

    // 7. CreateProcessW, suspended.
    let mut si = STARTUPINFOEXW {
        StartupInfo: STARTUPINFOW {
            cb: size_of::<STARTUPINFOEXW>() as u32,
            dwFlags: STARTF_USESTDHANDLES,
            hStdInput: h(&pipes.child_stdin),
            hStdOutput: h(&pipes.child_stdout),
            hStdError: h(&pipes.child_stderr),
            ..Default::default()
        },
        lpAttributeList: attrs.ptr(),
    };
    let app = wide(exe);
    let mut cmdline = wide(cmd);
    let dir = wide(folder);
    let mut pi = PROCESS_INFORMATION::default();
    // SAFETY: all buffers are NUL-terminated and outlive the call; `si` is a STARTUPINFOEXW with
    // `cb` set accordingly (EXTENDED_STARTUPINFO_PRESENT); `env` is a double-NUL-terminated UTF-16
    // block (CREATE_UNICODE_ENVIRONMENT); handles are inherited ONLY through HANDLE_LIST.
    let created = unsafe {
        CreateProcessW(
            PCWSTR(app.as_ptr()),
            Some(PWSTR(cmdline.as_mut_ptr())),
            None,
            None,
            true,
            EXTENDED_STARTUPINFO_PRESENT
                | CREATE_SUSPENDED
                | CREATE_NO_WINDOW
                | CREATE_UNICODE_ENVIRONMENT,
            Some(env.as_ptr() as *const core::ffi::c_void),
            PCWSTR(dir.as_ptr()),
            &mut si.StartupInfo as *mut STARTUPINFOW as *const STARTUPINFOW,
            &mut pi,
        )
    };
    drop(attrs);
    created.map_err(|_| fail("create_process"))?;
    let process = owned(pi.hProcess);
    let thread = owned(pi.hThread);

    // Job, then resume. A failure here terminates a child that never ran.
    // SAFETY: valid handles owned by us.
    let abort = |process: &OwnedHandle| {
        unsafe {
            let _ = TerminateProcess(h(process), 1);
        };
    };
    if unsafe { AssignProcessToJobObject(h(&job.0), h(&process)) }.is_err() {
        abort(&process);
        return Err(fail("job_assign"));
    }
    // SAFETY: valid thread handle of the suspended child.
    if unsafe { ResumeThread(h(&thread)) } == u32::MAX {
        abort(&process);
        return Err(fail("resume"));
    }
    drop(thread);
    // The child's pipe ends are closed in the parent so EOF propagates when it dies.
    let Pipes {
        child_stdin,
        parent_stdin,
        parent_stdout,
        child_stdout,
        parent_stderr,
        child_stderr,
    } = pipes;
    drop((child_stdin, child_stdout, child_stderr));
    Ok(WinProcess {
        streams: Some(ProcessStreams {
            stdin: Box::new(File::from(parent_stdin)),
            stdout: Box::new(File::from(parent_stdout)),
            stderr: Some(Box::new(File::from(parent_stderr))),
        }),
        process,
        job: Arc::new(job),
        _profile: profile,
    })
}

impl EngineProcess for WinProcess {
    fn take_streams(&mut self) -> Option<ProcessStreams> {
        self.streams.take()
    }

    fn kill_handle(&self) -> KillHandle {
        let job = self.job.clone();
        Arc::new(move || {
            // SAFETY: the Arc keeps the job handle open; terminating an already empty job is
            // harmless. Never waits.
            unsafe {
                let _ = TerminateJobObject(h(&job.0), 1);
            }
        })
    }

    fn wait(&mut self) -> Option<i32> {
        // SAFETY: valid process handle owned by `self`.
        let r = unsafe { WaitForSingleObject(h(&self.process), u32::MAX) };
        (r == WAIT_OBJECT_0).then_some(0)
    }
}
