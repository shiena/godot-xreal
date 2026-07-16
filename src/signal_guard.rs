//! XREAL SDK null-NativeGlasses crash workaround.
//!
//! `SessionManager::HandleActionCallback` (libXREALXRPlugin.so+0x849a8) reads
//! `SessionManager+0x60` (the `NativeGlasses*`) without a null check, and calls
//! `NativeGlasses::GetActionData(action_id)` on it.  When this pointer is null
//! (e.g. after DestroySession or on a thread where the TSingleton is not yet
//! initialised) the process crashes at `GetActionData+28/44` (fault addr 0x8).
//!
//! **Primary fix (Android)**: runtime code-patch of `HandleActionCallback+28`
//! (`bl NativeGlasses::GetActionData` at lib+0x849c4). We replace that 4-byte
//! BL with a BL to `null_safe_handle_action`, a small assembly trampoline that:
//!   - Loads NativeGlasses* from SessionManager+0x60 (replicating the original ldr).
//!   - If non-null: returns to 0x849c0 so `mov x19,x1; bl GetActionData` runs normally.
//!   - If null: advances lr by 12 (skipping `mov x19,x1`, `bl GetActionData`, and
//!     `mov x21,x0`), zeros x21 (null return value) and returns to 0x849cc so
//!     `LogHelper::Info` is called with (0, action_id) instead of crashing.
//!
//! The mprotect-based patch requires RWX permission on the code page.  On Android
//! this is usually allowed for system libraries in-process.
//!
//! **Secondary fix (SIGSEGV handler)**: kept as a fallback but note that Android
//! ART's `libsigchain` intercepts SIGSEGV *before* user sigaction handlers and
//! may terminate the process before our handler runs.  The code-patch is the
//! primary mechanism.

use std::sync::atomic::{AtomicUsize, Ordering};

// Runtime base address of libXREALXRPlugin.so — set by XrealNative::load().
static LIB_BASE: AtomicUsize = AtomicUsize::new(0);
// Set to 1 when our handler intercepts a crash, readable from the main thread.
static INTERCEPT_COUNT: AtomicUsize = AtomicUsize::new(0);

// Compile-time offsets inside libXREALXRPlugin.so (arm64-v8a, SDK v3.1.0):
//
//   Crash PC: `ldr x8, [x20, #0x8]` where x20 = null "this" → fault at 0x8.
//   This is at 0x8e238 (GetActionData+0x30 = +48). However libsigchain may subtract
//   4 from the PC in its backtrace and ALSO in the ucontext it presents to user handlers,
//   meaning the signal handler may see PC = 0x8e234 (GetActionData+0x2c = +44).
//   Check both to be robust.
//
//   Epilogue: `ldp x0,x1,[sp,#0x50]` ... `ret` at 0x8e3ec. The prologue has already:
//     - sub sp, #0xb0                          (frame allocated)
//     - stp x20,x19,[sp,#0xa0]                 (callee-saved regs saved)
//     - stp xzr,xzr,[sp,#0x50] at 0x8e234      (return value slot zeroed)
//   If the crash PC is 0x8e234 (before the zeroing instruction), we must zero [sp+0x50]
//   ourselves from the signal handler before redirecting to the epilogue.
const OFFSET_CRASH:    usize = 0x8e238; // ldr x8, [x20, #0x8] — actual faulting instruction
const OFFSET_CRASH_M4: usize = 0x8e234; // PC as reported by libsigchain (−4 offset)
const OFFSET_EPILOGUE: usize = 0x8e3ec; // ldp x0,x1,[sp,#0x50]; … ret

/// AArch64 trampoline that adds a null check before `NativeGlasses::GetActionData`.
///
/// Called via a patched BL at `HandleActionCallback+20` (lib+0x849bc).
/// This REPLACES `ldr x0, [x0, #0x60]` (the original load of NativeGlasses* from
/// SessionManager+0x60).
///
/// ABI contract at the patch site (0x849bc, before our wrapper):
///   x0  = SessionManager* (HandleActionCallback's `this`)
///   x1  = action_id
///   x30 = 0x849c0 (HandleActionCallback+0x18, the `mov x19, x1` instruction)
///
/// Non-null path: ret → x30=0x849c0; `mov x19,x1; bl GetActionData; mov x21,x0` run normally.
/// Null path: x30 += 12 → 0x849cc (= `adrp x0, 0x2b000` in LogHelper setup).
///   Skipped: `mov x19,x1` (0x849c0), `bl GetActionData` (0x849c4), `mov x21,x0` (0x849c8).
///   x21 = 0 prevents stale action-data in the subsequent LogHelper::Info call.
#[cfg(target_os = "android")]
core::arch::global_asm!(
    ".global null_safe_handle_action",
    ".type null_safe_handle_action, %function",
    "null_safe_handle_action:",
    "ldr x0, [x0, #0x60]",     // replicate the replaced ldr: x0 = NativeGlasses*
    "cbz x0, 1f",               // if null → 1f
    "ret",                       // non-null: return to 0x849c0 (proceed normally)
    "1:",
    "add x30, x30, #12",        // advance lr past: mov x19,x1 + bl GetActionData + mov x21,x0
    "mov x21, xzr",             // x21 = 0 (null action data)
    "ret",                       // return to 0x849cc
    ".size null_safe_handle_action, . - null_safe_handle_action"
);

#[cfg(target_os = "android")]
extern "C" {
    fn null_safe_handle_action();
}

/// Runtime code-patch: replace `ldr x0, [x0, #0x60]` at `HandleActionCallback+20`
/// (lib_base + 0x849bc) with `bl null_safe_handle_action`.
///
/// If the direct BL is out of ±128 MiB range, allocate a trampoline page via mmap
/// with a hint near lib_base so that the BL to the trampoline is in range, and the
/// trampoline does a long-range jump (LDR x17, +8; BR x17) to the actual wrapper.
#[cfg(target_os = "android")]
pub fn patch_handle_action_callback(lib_base: usize) {
    let patch_addr = lib_base + 0x849bc; // ldr x0,[x0,#0x60] = load NativeGlasses*

    let wrapper_addr = null_safe_handle_action as usize;
    let byte_offset  = wrapper_addr as i64 - patch_addr as i64;
    let word_offset  = byte_offset >> 2;
    let page_size: usize = 4096;

    let bl_target: usize = if word_offset.abs() <= (1i64 << 25) {
        wrapper_addr
    } else {
        // Out of ±128 MiB: allocate a trampoline page near lib_base.
        let hint = ((patch_addr & !0xFF_FFFF).wrapping_sub(0x400_0000)) as *mut libc::c_void;
        unsafe {
            let page = libc::mmap(
                hint, page_size,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0,
            );
            if page == libc::MAP_FAILED {
                godot::global::godot_print!("[xreal] code_patch: mmap trampoline failed");
                return;
            }
            // LDR X17, +8  (0x58000051) then BR X17 (0xD61F0220), then 8-byte literal
            let tram = page as *mut u32;
            *tram        = 0x5800_0051u32; // LDR x17, #8
            *tram.add(1) = 0xD61F_0220u32; // BR  x17
            *(tram.add(2) as *mut u64) = wrapper_addr as u64;
            let a = page as usize;
            core::arch::asm!("dc cvau,{a}","dsb ish","ic ivau,{a}","dsb ish","isb",a=in(reg)a);
            libc::mprotect(page, page_size, libc::PROT_READ | libc::PROT_EXEC);
            godot::global::godot_print!(
                "[xreal] code_patch: trampoline at {page:?} → {wrapper_addr:#018x}"
            );
            page as usize
        }
    };

    let bl_boff = bl_target as i64 - patch_addr as i64;
    let bl_woff = bl_boff >> 2;
    if bl_woff.abs() > (1i64 << 25) {
        godot::global::godot_print!(
            "[xreal] code_patch: bl_target still out of range ({bl_boff:#x}), giving up"
        );
        return;
    }
    let bl_insn: u32 = 0x9400_0000 | (bl_woff as u32 & 0x03FF_FFFF);
    let page_addr    = (patch_addr & !(page_size - 1)) as *mut libc::c_void;

    unsafe {
        if libc::mprotect(page_addr, page_size,
            libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC) != 0 {
            godot::global::godot_print!("[xreal] code_patch: mprotect(RWX) failed");
            return;
        }
        *(patch_addr as *mut u32) = bl_insn;
        core::arch::asm!(
            "dc cvau,{a}","dsb ish","ic ivau,{a}","dsb ish","isb",
            a = in(reg) patch_addr,
        );
        libc::mprotect(page_addr, page_size, libc::PROT_READ | libc::PROT_EXEC);
    }
    godot::global::godot_print!(
        "[xreal] code_patch: patched HandleActionCallback+20 at {patch_addr:#018x} \
         bl→{bl_target:#018x} encoding={bl_insn:#010x}"
    );
}

/// Runtime code-patch: replace `cbz w8, 0x6dd18` at `CreateDisplayLayer+0x80`
/// (lib_base + 0x6dc98) with `b 0x6dd18` (`0x14000020`) — force the **real `DisplayOverlay`**
/// branch instead of the `DummyDisplayOverlay` fall-through.
///
/// This branch is on `CreateDisplayLayer`'s **Multiview (`stereo_rendering_mode == 2`)** path only
/// (a stereo-mode split at lib+0x6dc50 sends Multipass down a separate two-overlay path that already
/// picks the real `DisplayOverlay`, so this patch is a no-op for Multipass). Target `0x6dd18` emplaces
/// `shared_ptr<DisplayOverlay>` into `DisplayManager+0x128`; the fall-through `0x6dc9c` emplaces a
/// `DummyDisplayOverlay`.
///
/// RE (codex + our cross-check, see `docs/codex-multiview-analysis.md`): the previous `cbz→nop`
/// forced the **dummy** in Multiview. `DummyDisplayOverlay::InitSwapchain @0x70e54` sets `overlay+0x8 = 1`
/// (and leaves the swapchain handle `overlay+0x18 == 0`); `OverlayBase::CreateBuffer` still runs
/// (dummy `GetRecommandBufferCount @0x70e60` = 1) so ONE array texture is created — but because
/// `overlay+0x8 != 0`, `PopulateNextFrameDesc` skips `OverlayBase::SetSwapChainBuffers`, so
/// `QueryTextureDesc` is never called and our texture is never registered with the NR swapchain →
/// black. The real `DisplayOverlay` (via `OverlayBase::InitSwapchain @0xa7fe0`) creates the swapchain,
/// leaves `overlay+0x8 == 0`, and lets `SetSwapChainBuffers` → `QueryTextureDesc` register the texture.
#[cfg(target_os = "android")]
pub fn patch_create_display_layer(lib_base: usize) {
    let patch_addr = lib_base + 0x6dc98; // cbz w8, 0x6dd18 → force `b 0x6dd18` (real DisplayOverlay)
    let branch_real: u32 = 0x1400_0020; // b 0x6dd18 (delta 0x80, imm26 0x20)
    let page_size: usize = 4096;
    let page_addr = (patch_addr & !(page_size - 1)) as *mut libc::c_void;

    unsafe {
        if libc::mprotect(
            page_addr,
            page_size,
            libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
        ) != 0
        {
            godot::global::godot_print!("[xreal] patch_display_layer: mprotect(RWX) failed");
            return;
        }
        *(patch_addr as *mut u32) = branch_real;
        core::arch::asm!(
            "dc cvau,{a}", "dsb ish", "ic ivau,{a}", "dsb ish", "isb",
            a = in(reg) patch_addr,
        );
        libc::mprotect(page_addr, page_size, libc::PROT_READ | libc::PROT_EXEC);
    }
    godot::global::godot_print!(
        "[xreal] patch_display_layer: patched CreateDisplayLayer at {patch_addr:#018x} \
         cbz→b 0x6dd18 (force real DisplayOverlay so Multiview registers the swapchain texture)"
    );
}

/// Runtime code-patch: replace the first instruction of `DisplayManager::UpdateMetrics()`
/// (lib_base + 0x68974) with `ret` (0xD65F03C0), turning it into a no-op.
///
/// `SubmitCurrentFrame` (needed to actually present our registered swapchain buffers) calls
/// `UpdateMetrics`, which dispatches through a metrics-reporter object at `DisplayManager+0x68`
/// (`blr [[DM+0x68]+0x18]`). Unity populates that reporter via a metrics provider; we do not, so the
/// callback is null and it crashes (SIGBUS, pc=0x13, at `UpdateMetrics+0x58`). `UpdateMetrics` only
/// reports frame telemetry (FramePresent / dropped-frame counts) — skipping it does not affect
/// presentation, which happens earlier in `SubmitCurrentFrame` via `SetBufferViewport` +
/// `NativeRendering::SubmitFrame`. Patching the entry to `ret` returns cleanly to `SubmitCurrentFrame`
/// (x30 still holds the return address, sp untouched).
#[cfg(target_os = "android")]
pub fn patch_update_metrics(lib_base: usize) {
    let patch_addr = lib_base + 0x68974; // DisplayManager::UpdateMetrics first instruction
    let ret: u32 = 0xD65F_03C0; // ret
    let page_size: usize = 4096;
    let page_addr = (patch_addr & !(page_size - 1)) as *mut libc::c_void;

    unsafe {
        if libc::mprotect(
            page_addr,
            page_size,
            libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
        ) != 0
        {
            godot::global::godot_print!("[xreal] patch_update_metrics: mprotect(RWX) failed");
            return;
        }
        *(patch_addr as *mut u32) = ret;
        core::arch::asm!(
            "dc cvau,{a}", "dsb ish", "ic ivau,{a}", "dsb ish", "isb",
            a = in(reg) patch_addr,
        );
        libc::mprotect(page_addr, page_size, libc::PROT_READ | libc::PROT_EXEC);
    }
    godot::global::godot_print!(
        "[xreal] patch_update_metrics: patched UpdateMetrics at {patch_addr:#018x} → ret \
         (skip null metrics callback so SubmitCurrentFrame can present)"
    );
}

/// Install the SIGSEGV guard. Call once with `libXREALXRPlugin.so`'s runtime base.
pub fn install(lib_base: usize) {
    LIB_BASE.store(lib_base, Ordering::SeqCst);

    #[cfg(target_os = "android")]
    {
        use libc::{SA_NODEFER, SA_RESTART, SA_SIGINFO};
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = sigsegv_handler as usize;
            sa.sa_flags = SA_SIGINFO | SA_RESTART | SA_NODEFER;
            libc::sigemptyset(&mut sa.sa_mask);
            let ret = libc::sigaction(libc::SIGSEGV, &sa, std::ptr::null_mut());
            godot::global::godot_print!(
                "[xreal] signal_guard: sigaction={ret}, lib_base={lib_base:#018x}, \
                 crash_addr={:#018x}, epilogue_addr={:#018x}",
                lib_base + OFFSET_CRASH,
                lib_base + OFFSET_EPILOGUE
            );
        }
    }
    #[cfg(not(target_os = "android"))]
    godot::global::godot_print!(
        "[xreal] signal_guard: no-op on non-Android (lib_base={lib_base:#018x})"
    );
}

#[cfg(target_os = "android")]
unsafe extern "C" fn sigsegv_handler(
    sig: libc::c_int,
    info: *mut libc::siginfo_t,
    ctx: *mut libc::c_void,
) {
    if info.is_null() || ctx.is_null() {
        restore_default(sig);
        return;
    }

    let fault_addr = (*info).si_addr() as usize;
    let lib_base   = LIB_BASE.load(Ordering::SeqCst);

    // Only intercept the specific null+8 dereference from GetActionData.
    // Always write something to a file to confirm the handler was called at all.
    // (write() to stderr may not appear in logcat on Android)
    {
        let path = b"/data/local/tmp/xreal_guard.txt\0";
        let fd = libc::open(
            path.as_ptr() as *const libc::c_char,
            libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
            0o644,
        );
        if fd >= 0 {
            let msg = format!("handler called fault={fault_addr:#x} lib_base={lib_base:#x}\n");
            libc::write(fd, msg.as_ptr() as *const libc::c_void, msg.len());
            libc::close(fd);
        }
    }

    if fault_addr == 0x8 && lib_base != 0 {
        // Access PC via known Bionic AArch64 ucontext offset to avoid struct layout issues.
        //
        // Bionic ucontext_t (AArch64, LP64):
        //   +0x00  uc_flags   (u64)
        //   +0x08  uc_link    (u64)
        //   +0x10  uc_stack   (24 bytes: ss_sp u64, ss_flags i32, pad i32, ss_size u64)
        //   +0x28  uc_sigmask (u64 = 8 bytes — Bionic sigset_t on LP64)
        //   +0x30  padding    (120 bytes, fills to 128 for sigmask area)
        //   +0xa8  uc_mcontext (sigcontext)
        //     +0xa8  fault_address (u64)
        //     +0xb0  regs[0..30]   (31×8 = 248 bytes)
        //     +0x1a8 sp            (u64)
        //     +0x1b0 pc            (u64)  ← target
        const PC_OFFSET_IN_UCONTEXT: usize = 0x1b0;
        let pc_ptr = (ctx as *mut u8).add(PC_OFFSET_IN_UCONTEXT) as *mut u64;
        let pc     = *pc_ptr as usize;
        let tgt    = lib_base + OFFSET_CRASH;

        // Use async-signal-safe write() to log crash PC (godot_print! is not signal-safe).
        let pc_offset = pc.wrapping_sub(lib_base);
        let msg = format!(
            "[xreal] signal_guard: SIGSEGV fault={fault_addr:#x} pc={pc:#018x} \
             tgt={tgt:#018x} offset={pc_offset:#010x}\n"
        );
        unsafe { libc::write(2, msg.as_ptr() as *const libc::c_void, msg.len()) };

        // Accept both the raw faulting PC (0x8e238) and the -4 variant that libsigchain
        // may present to user handlers (0x8e234).
        let tgt_m4 = lib_base + OFFSET_CRASH_M4;
        if pc == tgt || pc == tgt_m4 {
            let epi = lib_base + OFFSET_EPILOGUE;
            let msg2 = format!(
                "[xreal] signal_guard: intercepting pc={pc:#018x} -> epilogue={epi:#018x}\n"
            );
            unsafe { libc::write(2, msg2.as_ptr() as *const libc::c_void, msg2.len()) };
            INTERCEPT_COUNT.fetch_add(1, Ordering::SeqCst);

            // If we're at 0x8e234 (before `stp xzr,xzr,[sp,#0x50]`), the return-value slot
            // [sp+0x50..0x5f] hasn't been zeroed yet.  Write zeros now so the epilogue's
            // `ldp x0, x1, [sp, #0x50]` returns (0, 0) instead of stack garbage.
            //   sp is at ctx + 0x1a8 (Bionic AArch64 ucontext layout, see above).
            if pc == tgt_m4 {
                const SP_OFFSET_IN_UCONTEXT: usize = 0x1a8;
                let sp = *((ctx as *const u8).add(SP_OFFSET_IN_UCONTEXT) as *const u64) as usize;
                let ret_slot = (sp + 0x50) as *mut u64;
                *ret_slot                = 0;
                *ret_slot.add(1)         = 0;
            }

            // Redirect PC to the function's epilogue.
            *pc_ptr = epi as u64;
            return; // Resume at epilogue — stack frame is intact, callee-saves were saved
        }
    }

    restore_default(sig);
}

#[cfg(target_os = "android")]
unsafe fn restore_default(sig: libc::c_int) {
    libc::signal(sig, libc::SIG_DFL);
    libc::raise(sig);
}
