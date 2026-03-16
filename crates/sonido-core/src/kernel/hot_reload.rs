//! Hot-reload kernel support via cdylib swap.
//!
//! Compile a kernel to cdylib, load via dlopen, swap function pointer
//! in `Adapter<K, SmoothedPolicy>` while preserving smoother state. Development mode:
//! `cargo watch` triggers recompile + hot-swap on file save.
//!
//! # Workflow
//!
//! 1. Build kernel crate as `cdylib` (add `[lib] crate-type = ["cdylib"]`).
//! 2. Construct a [`HotReloadConfig`] pointing at the build output directory.
//! 3. Load via `HotKernel::load` (not yet implemented — returns placeholder).
//! 4. Swap into a running `Adapter` without restarting the audio thread.
//!
//! Smoother state is preserved across swaps so parameter changes remain glitch-free.
//!
//! Only applicable to `Adapter<K, SmoothedPolicy>` — embedded (`DirectPolicy`) targets
//! do not use software smoothers.
//!
//! # Status
//!
//! Types defined. Runtime loading (`dlopen` / `libloading`) not yet implemented.

/// Handle to a dynamically loaded kernel.
///
/// Wraps a native shared library handle, resolved function pointers for the
/// [`DspKernel`](crate::kernel::DspKernel) vtable, and a monotone version tag
/// used to sequence concurrent swap requests.
///
/// # Invariants
///
/// - `_private` will be replaced by the live fields once `libloading` is wired in.
/// - The handle must be kept alive for as long as any audio thread calls the
///   function pointers it provides.
pub struct HotKernel {
    // TODO: libloading::Library handle, resolved fn pointers, version tag (u64)
    _private: (),
}

impl HotKernel {
    /// Load a kernel from a compiled cdylib at `lib_dir/<kernel_name>.<ext>`.
    ///
    /// Returns `None` until the runtime loading backend is implemented.
    ///
    /// # Arguments
    ///
    /// * `config` — Describes where to find the library and which kernel to load.
    ///
    /// # Status
    ///
    /// TODO: implement via `libloading` crate.
    #[allow(unused_variables)]
    pub fn load(config: &HotReloadConfig) -> Option<Self> {
        // TODO: dlopen config.lib_dir/<kernel_name>.<ext>, resolve symbols
        None
    }

    /// Returns the monotone version tag for ordering concurrent swap requests.
    ///
    /// Incremented each time a new library is successfully loaded. Callers can
    /// compare tags to detect whether a swap is still pending.
    pub fn version(&self) -> u64 {
        // TODO: return internal version counter
        0
    }
}

/// Configuration for the hot-reload watcher.
///
/// Passed to [`HotKernel::load`] and (eventually) a background file-watch task
/// that polls for library changes and issues automatic swaps.
pub struct HotReloadConfig {
    /// Path to the directory containing the compiled cdylib output.
    ///
    /// Typically `target/debug/` or a custom `--out-dir` path.
    pub lib_dir: &'static str,

    /// Base name of the kernel to watch (without platform extension).
    ///
    /// The loader appends `.so` (Linux), `.dylib` (macOS), or `.dll` (Windows).
    pub kernel_name: &'static str,

    /// How often to poll the output directory for a changed mtime, in ms.
    ///
    /// Valid range: 50–5000 ms. Typical: 200 ms.
    pub poll_ms: u32,
}
