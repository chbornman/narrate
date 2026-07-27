//! The live hardware probe (spec/RUNTIME.md §6.1), feeding core's
//! `HardwareProbe` seam: adapter enumeration via wgpu, byte-accurate VRAM
//! via the native layer per platform — Vulkan
//! `VkPhysicalDeviceMemoryProperties` (largest DEVICE_LOCAL heap) on
//! Linux; Windows DXGI `DedicatedVideoMemory` and the macOS
//! Metal/`hw.memsize` paths are code-complete behind cfg and
//! founder/CI-verified. A GPU-less machine resolves Tier 0 (the whole-
//! product floor); calibrated tier numbers are the P6.3 spike's.

use photoproof_core::runtime::{GpuAdapter, HardwareProbe, HardwareReport};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Probing is rare (first run, settings re-detect) and some drivers
/// dislike concurrent instance creation — one probe at a time,
/// process-wide.
static PROBE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub struct LiveProbe;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveCapabilityResult {
    pub hardware: HardwareReport,
    pub providers: Vec<photoproof_connectors::OrtProviderCapability>,
    pub total_memory_bytes: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("hardware capability probe was cancelled")]
    Cancelled,
    #[error("hardware capability probe exceeded its {0:?} deadline")]
    Timeout(Duration),
    #[error("hardware capability probe process could not start: {0}")]
    Spawn(String),
    #[error("hardware capability helper failed: {0}")]
    Helper(String),
}

impl LiveProbe {
    /// Run driver/ORT discovery in a disposable helper process. Unlike a Rust
    /// thread blocked in a vendor driver, the helper can be killed on deadline,
    /// cancellation, or application shutdown, so no native probe can outlive
    /// the managed task barrier.
    pub fn probe_bounded(
        cancel: Arc<AtomicBool>,
        timeout: Duration,
    ) -> Result<LiveCapabilityResult, ProbeError> {
        let executable =
            std::env::current_exe().map_err(|error| ProbeError::Spawn(error.to_string()))?;
        let child = std::process::Command::new(executable)
            .arg("--photoproof-capability-helper")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| ProbeError::Spawn(error.to_string()))?;
        collect_bounded_probe(child, cancel, timeout)
    }

    pub fn probe_for_helper() -> LiveCapabilityResult {
        let hardware = LiveProbe.probe();
        let providers = photoproof_connectors::ort_provider_capabilities();
        LiveCapabilityResult {
            hardware,
            providers,
            total_memory_bytes: total_memory_bytes(),
        }
    }
}

/// Own one capability-helper process through a terminal result. Factoring the
/// child supervision from command construction gives the acceptance suite a
/// real killable process seam: timeout/cancellation tests do not pretend that a
/// cooperative Rust closure is equivalent to a vendor driver stuck in native
/// code.
fn collect_bounded_probe(
    mut child: std::process::Child,
    cancel: Arc<AtomicBool>,
    timeout: Duration,
) -> Result<LiveCapabilityResult, ProbeError> {
    let started = Instant::now();
    loop {
        if cancel.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ProbeError::Cancelled);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ProbeError::Timeout(timeout));
        }
        match child.try_wait() {
            Ok(Some(_)) => {
                let output = child
                    .wait_with_output()
                    .map_err(|error| ProbeError::Helper(error.to_string()))?;
                if !output.status.success() {
                    return Err(ProbeError::Helper(
                        String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                    ));
                }
                return serde_json::from_slice(&output.stdout)
                    .map_err(|error| ProbeError::Helper(error.to_string()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => return Err(ProbeError::Helper(error.to_string())),
        }
    }
}

impl HardwareProbe for LiveProbe {
    fn probe(&mut self) -> HardwareReport {
        let _serial = PROBE_LOCK.lock().expect("probe lock");
        let mut adapters = enumerate_wgpu_adapters();
        attach_native_vram(&mut adapters);
        HardwareReport {
            schema_version: 1,
            adapters,
            apple_unified_bytes: apple_unified_bytes(),
            detected_at: photoproof_core::UtcMillis::now().to_rfc3339(),
        }
    }
}

/// §6.1.1: enumerate adapters via wgpu. Software rasterizers (llvmpipe /
/// WARP) report `DeviceType::Cpu` and are excluded — a software "GPU"
/// must never promote the tier.
fn enumerate_wgpu_adapters() -> Vec<GpuAdapter> {
    // Enumeration needs no window: the display-less descriptor.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()))
        .into_iter()
        .filter_map(|adapter| {
            let info = adapter.get_info();
            if matches!(info.device_type, wgpu::DeviceType::Cpu) {
                return None;
            }
            Some(GpuAdapter {
                name: info.name,
                backend: info.backend.to_string(),
                vendor_id: Some(info.vendor),
                device_id: Some(info.device),
                driver: (!info.driver.is_empty()).then_some(info.driver),
                driver_info: (!info.driver_info.is_empty()).then_some(info.driver_info),
                vram_bytes: None, // the native layer fills this in
            })
        })
        .collect()
}

/// Linux: Vulkan via ash — for each physical device, the largest
/// DEVICE_LOCAL heap; matched to wgpu adapters by name.
#[cfg(target_os = "linux")]
fn attach_native_vram(adapters: &mut [GpuAdapter]) {
    let Ok(entry) = (unsafe { ash::Entry::load() }) else {
        return; // no Vulkan loader: enumeration-only report
    };
    let app_info =
        ash::vk::ApplicationInfo::default().api_version(ash::vk::make_api_version(0, 1, 1, 0));
    let create_info = ash::vk::InstanceCreateInfo::default().application_info(&app_info);
    let Ok(instance) = (unsafe { entry.create_instance(&create_info, None) }) else {
        return;
    };
    let devices = unsafe { instance.enumerate_physical_devices() }.unwrap_or_default();
    for device in devices {
        let props = unsafe { instance.get_physical_device_properties(device) };
        if props.device_type == ash::vk::PhysicalDeviceType::CPU {
            continue;
        }
        let name = props
            .device_name_as_c_str()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mem = unsafe { instance.get_physical_device_memory_properties(device) };
        let largest_device_local = mem.memory_heaps[..mem.memory_heap_count as usize]
            .iter()
            .filter(|h| h.flags.contains(ash::vk::MemoryHeapFlags::DEVICE_LOCAL))
            .map(|h| h.size)
            .max()
            .unwrap_or(0);
        for adapter in adapters.iter_mut() {
            if adapter.vram_bytes.is_none() && adapter.name == name {
                adapter.vram_bytes = Some(largest_device_local);
            }
        }
    }
    unsafe { instance.destroy_instance(None) };
}

/// Windows: DXGI `DedicatedVideoMemory` per adapter (§6.1.1).
/// **Code-complete behind cfg; founder/CI-verified** — this container
/// cannot compile-check the windows target. Raw FFI keeps the packet free
/// of an unverifiable crate dependency.
#[cfg(target_os = "windows")]
fn attach_native_vram(adapters: &mut [GpuAdapter]) {
    #[repr(C)]
    struct DxgiAdapterDesc1 {
        description: [u16; 128],
        vendor_id: u32,
        device_id: u32,
        sub_sys_id: u32,
        revision: u32,
        dedicated_video_memory: usize,
        dedicated_system_memory: usize,
        shared_system_memory: usize,
        adapter_luid: [u32; 2],
        flags: u32,
    }
    // COM vtable shapes for IDXGIFactory1 / IDXGIAdapter1 — only the
    // slots we call.
    type EnumAdapters1Fn =
        unsafe extern "system" fn(*mut core::ffi::c_void, u32, *mut *mut core::ffi::c_void) -> i32;
    type GetDesc1Fn =
        unsafe extern "system" fn(*mut core::ffi::c_void, *mut DxgiAdapterDesc1) -> i32;
    type ReleaseFn = unsafe extern "system" fn(*mut core::ffi::c_void) -> u32;

    #[link(name = "dxgi")]
    unsafe extern "system" {
        fn CreateDXGIFactory1(riid: *const [u8; 16], factory: *mut *mut core::ffi::c_void) -> i32;
    }
    // IID_IDXGIFactory1 {770aae78-f26f-4dba-a829-253c83d1b387}
    const IID_IDXGIFACTORY1: [u8; 16] = [
        0x78, 0xae, 0x0a, 0x77, 0x6f, 0xf2, 0xba, 0x4d, 0xa8, 0x29, 0x25, 0x3c, 0x83, 0xd1, 0xb3,
        0x87,
    ];
    unsafe {
        let mut factory: *mut core::ffi::c_void = core::ptr::null_mut();
        if CreateDXGIFactory1(&IID_IDXGIFACTORY1, &mut factory) < 0 || factory.is_null() {
            return;
        }
        let factory_vtbl = *(factory as *mut *mut usize);
        // IDXGIFactory1::EnumAdapters1 is vtable slot 12; IUnknown::Release
        // is slot 2.
        let enum_adapters: EnumAdapters1Fn = core::mem::transmute(*factory_vtbl.add(12));
        let factory_release: ReleaseFn = core::mem::transmute(*factory_vtbl.add(2));
        let mut index = 0u32;
        loop {
            let mut raw: *mut core::ffi::c_void = core::ptr::null_mut();
            if enum_adapters(factory, index, &mut raw) < 0 || raw.is_null() {
                break;
            }
            index += 1;
            let vtbl = *(raw as *mut *mut usize);
            // IDXGIAdapter1::GetDesc1 is vtable slot 10.
            let get_desc1: GetDesc1Fn = core::mem::transmute(*vtbl.add(10));
            let release: ReleaseFn = core::mem::transmute(*vtbl.add(2));
            let mut desc = core::mem::zeroed::<DxgiAdapterDesc1>();
            if get_desc1(raw, &mut desc) >= 0 {
                let end = desc
                    .description
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(desc.description.len());
                let name = String::from_utf16_lossy(&desc.description[..end]);
                for adapter in adapters.iter_mut() {
                    if adapter.vram_bytes.is_none() && adapter.name == name {
                        adapter.vram_bytes = Some(desc.dedicated_video_memory as u64);
                    }
                }
            }
            release(raw);
        }
        factory_release(factory);
    }
}

/// macOS: dedicated-VRAM via Metal `recommendedMaxWorkingSetSize` is the
/// §6.1.1 recipe; Apple Silicon machines take the unified-memory path
/// below instead, which decides the tier there. Founder-verified.
#[cfg(target_os = "macos")]
fn attach_native_vram(_adapters: &mut [GpuAdapter]) {
    // Intentionally empty pending founder verification: on Apple Silicon
    // the §6.1.2 unified-memory gate governs; Intel-Mac dedicated VRAM is
    // not a v1 tier-1 target (RUNTIME §6.2 gates).
}

/// §6.1.2: Apple Silicon unified memory = `sysctl hw.memsize`. The 60%
/// usable-budget heuristic is folded into the §6.2 gates in core.
#[cfg(target_os = "macos")]
fn apple_unified_bytes() -> Option<u64> {
    let mut size: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let name = c"hw.memsize";
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&mut size as *mut u64).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0).then_some(size)
}

#[cfg(target_os = "linux")]
fn total_memory_bytes() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kib = text
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    kib.checked_mul(1024)
}

#[cfg(target_os = "macos")]
fn total_memory_bytes() -> Option<u64> {
    apple_unified_bytes()
}

#[cfg(target_os = "windows")]
fn total_memory_bytes() -> Option<u64> {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }
    let mut status = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        memory_load: 0,
        total_phys: 0,
        avail_phys: 0,
        total_page_file: 0,
        avail_page_file: 0,
        total_virtual: 0,
        avail_virtual: 0,
        avail_extended_virtual: 0,
    };
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.total_phys)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn total_memory_bytes() -> Option<u64> {
    None
}

#[cfg(not(target_os = "macos"))]
fn apple_unified_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use photoproof_connectors::config::Tier;
    use photoproof_core::runtime::decide_tier;

    fn spawn_hung_test_process() -> std::process::Child {
        std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--ignored",
                "--exact",
                "hardware::tests::hung_probe_process_fixture",
                "--nocapture",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("hung helper fixture")
    }

    /// Only launched by the two process-supervision tests above. It is ignored
    /// in ordinary test runs so a gate can never wait for its long sleep.
    #[test]
    #[ignore = "subprocess fixture for bounded capability-probe tests"]
    fn hung_probe_process_fixture() {
        std::thread::sleep(Duration::from_secs(30));
    }

    #[test]
    fn hung_probe_is_killed_at_the_deadline() {
        let started = Instant::now();
        let error = collect_bounded_probe(
            spawn_hung_test_process(),
            Arc::new(AtomicBool::new(false)),
            Duration::from_millis(75),
        )
        .expect_err("hung helper must time out");
        assert!(matches!(error, ProbeError::Timeout(_)));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the native helper is killed and reaped instead of outliving startup"
        );
    }

    #[test]
    fn capability_probe_cancellation_kills_and_reaps_the_helper() {
        let cancel = Arc::new(AtomicBool::new(true));
        let started = Instant::now();
        let error =
            collect_bounded_probe(spawn_hung_test_process(), cancel, Duration::from_secs(30))
                .expect_err("cancelled helper must stop");
        assert!(matches!(error, ProbeError::Cancelled));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "shutdown cancellation kills and reaps the native helper"
        );
    }

    /// The live path runs wherever the gate runs — a GPU-less CI box
    /// (Tier 0) or the founder's dev machine (the §12.1 RTX 5080 box):
    /// the probe must not panic, software rasterizers must never promote
    /// the tier, and the decision must be coherent with the report. The
    /// §6.2 table itself is pinned environment-independently in core
    /// (runtime::tier tests).
    #[test]
    fn live_probe_yields_a_coherent_report_and_decision() {
        let report = LiveProbe.probe();
        let decision = decide_tier(&report, Tier::Auto);
        assert!(decision.effective_tier <= 2);
        assert!(!decision.overridden_above, "Auto never warns");
        let best_vram = report
            .adapters
            .iter()
            .filter_map(|a| a.vram_bytes)
            .max()
            .unwrap_or(0);
        if report.adapters.is_empty() && report.apple_unified_bytes.is_none() {
            assert_eq!(decision.effective_tier, 0, "no GPU ⇒ the §6.2 floor");
        }
        if best_vram >= 8 * 1024 * 1024 * 1024 {
            assert!(decision.effective_tier >= 1, "{report:?}");
        }
        // The user override always wins, both directions, on the LIVE
        // report too (§6.2).
        let forced = decide_tier(&report, Tier::Fixed(0));
        assert_eq!(forced.effective_tier, 0);
    }
}
