//! Windows machine probe (S3-4): installed RAM from `GlobalMemoryStatusEx`, the GPU with the most
//! dedicated video memory from DXGI (`IDXGIFactory1::EnumAdapters1`, software adapters skipped).
//! Read-only system queries, no window, no capture, no network. Any failure leaves that field
//! `None`.
//!
//! UNTESTED on this machine: compile-checked for windows-gnu only (docs/wizard.md).

use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

use super::probe::MachineProbe;
use super::state::MachineInfo;

pub struct WindowsMachineProbe;

const MIB: u64 = 1024 * 1024;

fn to_mb(bytes: u64) -> Option<u32> {
    u32::try_from(bytes / MIB).ok()
}

fn ram_mb() -> Option<u32> {
    let mut st = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `st` is a valid MEMORYSTATUSEX with `dwLength` set.
    unsafe { GlobalMemoryStatusEx(&mut st) }.ok()?;
    to_mb(st.ullTotalPhys)
}

/// `(name, dedicated video memory in MiB)` of the best hardware adapter.
fn best_gpu() -> Option<(String, Option<u32>)> {
    // SAFETY: plain DXGI factory creation and adapter enumeration; every returned interface is
    // owned and released by the `windows` crate wrappers.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let mut best: Option<(String, u64)> = None;
        let mut i = 0;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            i += 1;
            let Ok(desc) = adapter.GetDesc1() else {
                continue;
            };
            if desc.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0 {
                continue;
            }
            let end = desc
                .Description
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(desc.Description.len());
            let name = String::from_utf16_lossy(&desc.Description[..end]);
            let vram = desc.DedicatedVideoMemory as u64;
            if best.as_ref().is_none_or(|(_, v)| vram > *v) {
                best = Some((name, vram));
            }
        }
        best.map(|(n, v)| (n, to_mb(v)))
    }
}

impl MachineProbe for WindowsMachineProbe {
    fn probe(&self) -> MachineInfo {
        let gpu = best_gpu();
        MachineInfo {
            ram_mb: ram_mb(),
            vram_mb: gpu.as_ref().and_then(|g| g.1),
            gpu_name: gpu.map(|g| g.0),
        }
        .sanitized()
    }
}
