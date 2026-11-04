//! `buddy-agent`: the sandboxed plan engine. Speaks the stdio JSON-lines protocol of ADR 0003
//! section 3 and nothing else: no files, no network, no arguments, no environment.

use std::process::ExitCode;

fn main() -> ExitCode {
    if !harden_dll_search() {
        return ExitCode::from(3);
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    match agent::child::run_engine_main(stdin, stdout) {
        agent::child::ChildExit::Done | agent::child::ChildExit::Cancelled => ExitCode::SUCCESS,
        agent::child::ChildExit::Failed => ExitCode::from(2),
    }
}

/// Windows: only System32 and the application directory are searched for DLLs (ADR 0003
/// section 3.6). Compile-checked only; see docs/manual-tests.md.
#[cfg(windows)]
fn harden_dll_search() -> bool {
    use windows::Win32::System::LibraryLoader::{
        SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_APPLICATION_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
    };
    // SAFETY: a plain process-wide setting with constant flags.
    // Failure ends the process (fail closed).
    unsafe {
        SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32 | LOAD_LIBRARY_SEARCH_APPLICATION_DIR)
            .is_ok()
    }
}

#[cfg(not(windows))]
fn harden_dll_search() -> bool {
    true
}
