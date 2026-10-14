// muguet - sysmon.rs
// System health monitor (iOS only): memory headroom timeline, memory warnings,
// thermal state and Low Power Mode. Logging only for now; the atomics below are
// the hook for later adaptive behaviour. Nothing here is a measurement until it
// has been read off a real device log.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::NSString;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::time::Duration;

extern "C" {
    fn os_proc_available_memory() -> usize;
}

/// Number of UIApplicationDidReceiveMemoryWarning notifications seen so far.
pub static MEMORY_WARNINGS: AtomicU64 = AtomicU64::new(0);
/// NSProcessInfoThermalState: 0 nominal, 1 fair, 2 serious, 3 critical.
pub static THERMAL_STATE: AtomicIsize = AtomicIsize::new(0);
pub static LOW_POWER: AtomicBool = AtomicBool::new(false);

static STARTED: AtomicBool = AtomicBool::new(false);

fn thermal_name(s: isize) -> &'static str {
    match s {
        0 => "nominal",
        1 => "fair",
        2 => "serious",
        3 => "critical",
        _ => "unknown",
    }
}

fn read_process_info() -> (isize, bool) {
    unsafe {
        let pi: Retained<AnyObject> = msg_send![class!(NSProcessInfo), processInfo];
        let thermal: isize = msg_send![&*pi, thermalState];
        let low: bool = msg_send![&*pi, isLowPowerModeEnabled];
        (thermal, low)
    }
}

fn observe_memory_warnings() {
    unsafe {
        let center: Retained<AnyObject> = msg_send![class!(NSNotificationCenter), defaultCenter];
        let name = NSString::from_str("UIApplicationDidReceiveMemoryWarningNotification");
        let block = RcBlock::new(|_note: *mut AnyObject| {
            let n = MEMORY_WARNINGS.fetch_add(1, Ordering::Relaxed) + 1;
            eprintln!(
                "[sysmon] MEMORY WARNING #{n}: footprint {} MB, iOS says {} MB still available",
                crate::footprint_mb(),
                os_proc_available_memory() >> 20
            );
        });
        let null: *const AnyObject = std::ptr::null();
        let _token: Retained<AnyObject> =
            msg_send![&*center, addObserverForName: &*name, object: null, queue: null, usingBlock: &*block];
        // The center keeps its own copy of the block and the token for the process lifetime.
        std::mem::forget(_token);
    }
}

pub fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    observe_memory_warnings();
    std::thread::Builder::new()
        .name("sysmon".into())
        .spawn(|| {
            let mut min_avail = usize::MAX;
            let mut tick = 0u64;
            loop {
                let avail = unsafe { os_proc_available_memory() };
                min_avail = min_avail.min(avail);
                let (thermal, low) = read_process_info();
                let changed = thermal != THERMAL_STATE.swap(thermal, Ordering::Relaxed) || low != LOW_POWER.swap(low, Ordering::Relaxed);
                if changed || tick % 10 == 0 {
                    let heap_mb = crate::RT.get().map(|r| r.mem.data_size() >> 20).unwrap_or(0);
                    eprintln!(
                        "[sysmon] t+{}s footprint {} MB, available {} MB (lowest so far {} MB), wasm heap {} MB, warnings {}, thermal {}, low power {}",
                        tick * 3,
                        crate::footprint_mb(),
                        avail >> 20,
                        min_avail >> 20,
                        heap_mb,
                        MEMORY_WARNINGS.load(Ordering::Relaxed),
                        thermal_name(thermal),
                        low
                    );
                }
                tick += 1;
                std::thread::sleep(Duration::from_secs(3));
            }
        })
        .ok();
}
