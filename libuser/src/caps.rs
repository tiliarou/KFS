//! Kernel Capabilities declaration
//!
//! Every program loaded by Sunrise has to declare the kernel capabilities it wishes
//! to use. Upon doing a privileged action (such as using a syscall, or creating
//! an event for an IRQ), the kernel will check that the process was allowed to
//! take this action.
//!
//! The main use-case is to make privilege escalation more complicated. In Sunrise,
//! an exploit only grants the capabilities of the process that was vulnerable,
//! requiring more pivoting in order to gain better accesses. For instance,
//! a vulnerability in the browser does not give rights to access the filesystem.
//!
//! Programs declare their capabilities by putting them in the .kernel_caps
//! section of their ELF executable. Each capability is encoded on an u32. We
//! provide convenience functions to generate those capabilities. Most programs
//! will want to use the `capabilities!` macro in order to generate this section.
//!
//! The capabilities macro takes two arrays: the first contains a list of syscall
//! numbers, and the second contains a list of raw capabilities. Syscalls are
//! handled specially in order to make them easier to declare.
//!
//! In addition, programs are expected to provide a `KipHeader`, telling the
//! kernel various information about how to start the process - such as how much
//! memory it should allocate for the stack, or what the default priority of the
//! process is. Those are provided by the `kip_header` macro.
//!
//! # Example
//!
//! ```
//! extern crate sunrise_libuser;
//! use sunrise_libuser::{syscalls, caps, capabilities, kip_header};
//! use sunrise_libuser::caps::ProcessCategory;
//! kip_header!(HEADER = caps::KipHeader {
//!     magic: *b"KIP1",
//!     name: *b"test\0\0\0\0\0\0\0\0",
//!     title_id: 0x0200000000001000,
//!     process_category: ProcessCategory::KernelBuiltin,
//!     main_thread_priority: 0,
//!     default_cpu_core: 0,
//!     reserved: 0,
//!     flags: 0,
//!     stack_page_count: 16,
//! });
//!
//! capabilities!(CAPABILITIES = Capabilities {
//!     svcs: [
//!         syscalls::nr::SetHeapSize,
//!         syscalls::nr::QueryMemory,
//!         syscalls::nr::ExitProcess,
//!         syscalls::nr::CreateThread,
//!         syscalls::nr::StartThread,
//!         syscalls::nr::ExitThread,
//!         syscalls::nr::MapSharedMemory,
//!         syscalls::nr::UnmapSharedMemory,
//!         syscalls::nr::CloseHandle,
//!         syscalls::nr::WaitSynchronization,
//!         syscalls::nr::ConnectToNamedPort,
//!         syscalls::nr::SendSyncRequestWithUserBuffer,
//!         syscalls::nr::OutputDebugString,
//!         syscalls::nr::CreateSharedMemory,
//!         syscalls::nr::CreateInterruptEvent,
//!         syscalls::nr::SleepThread
//!     ],
//!     raw_caps: [caps::ioport(0x60), caps::ioport(0x64), caps::irq_pair(1, 0x3FF)],
//! });
//! ```

/// Define the capabilities array in the .kernel_caps section. Has the following
/// syntax:
///
/// ```no_run
/// extern crate sunrise_libuser;
/// use sunrise_libuser::{syscalls, caps, capabilities};
/// capabilities!(CAPABILITIES = Capabilities {
///     svcs: [
///         // Array of syscall numbers.
///         syscalls::nr::SetHeapSize,
///         syscalls::nr::ExitProcess,
///     ],
///     raw_caps: [
///          // Array of raw kernel capabilities.
///          caps::ioport(0x60), caps::irq_pair(1, 0x3FF)
///     ]
/// });
/// ```
///
/// The order of the capabilities member is not important, and trailing comas are allowed.
#[macro_export]
macro_rules! capabilities {
    ($ident:ident = Capabilities {
        $($item:tt : [$($itemval:expr),* $(,)*]),* $(,)*
    }) => {
        capabilities!(@handle_item $ident, curcount=6, svcs=[], rawcaps=[], $($item: [$($itemval),*],)*);
    };
    (@handle_item $ident:ident, curcount=$count:expr, svcs=[], rawcaps=[$($rawcaps:expr),*],
      svcs: [$($svcs:expr),*], $($next:tt : [$($nextval:expr),*],)*) =>
    {
        capabilities!(@handle_item $ident, curcount=$count, svcs=[$($svcs),*], rawcaps=[$($rawcaps),*], $($next: [$($nextval),*],)*);
    };
    (@handle_item $ident:ident, curcount=$count:expr, svcs=[$($svcs:expr),*], rawcaps=[],
      raw_caps: [$($raw_caps:expr),*], $($next:tt : [$($nextval:expr),*],)*) =>
    {
        capabilities!(@handle_item $ident, curcount=$count + capabilities!(@count_elems $($raw_caps,)*),
                      svcs=[$($svcs),*], rawcaps=[$($raw_caps),*], $(next: [$($nextval),*],)*);
    };
    (@handle_item $ident:ident, curcount=$count:expr, svcs=[$($svcs:expr),*], rawcaps=[$($raw_caps:expr),*],) => {
        #[cfg_attr(not(test), link_section = ".kernel_caps")]
        #[used]
        static $ident: [u32; $count] = {
            let mut kacs = [
                // first 6 are SVCs
                0 << 29 | 0b1111,
                1 << 29 | 0b1111,
                2 << 29 | 0b1111,
                3 << 29 | 0b1111,
                4 << 29 | 0b1111,
                5 << 29 | 0b1111,
                $($raw_caps,)*
            ];
            capabilities!(@generate_svc kacs, [$($svcs),*]);

            kacs
        };
    };
    (@generate_svc $kac_svcs:ident, [$($svc:expr),*]) => {
        $($kac_svcs[$svc / 24] |= 1 << (($svc % 24) + 5);)*
    };
    (@count_elems) => {
        0
    };
    (@count_elems $val:expr, $($vals:expr,)*) => {
        1 + capabilities!(@count_elems $($vals,)*)
    };
}

pub use sunrise_libkern::process::{KipHeader, ProcessCategory};

/// Define the kernel built-ins in the .kip_header section. Has the following
/// syntax:
///
/// ```no_run
/// extern crate sunrise_libuser;
/// use sunrise_libuser::{caps, kip_header};
/// use sunrise_libuser::caps::ProcessCategory;
/// kip_header!(HEADER = caps::KipHeader {
///     magic: *b"KIP1",
///     name: *b"test\0\0\0\0\0\0\0\0",
///     title_id: 0x0200000000001000,
///     process_category: ProcessCategory::KernelBuiltin,
///     main_thread_priority: 0,
///     default_cpu_core: 0,
///     flags: 0,
///     reserved: 0,
///     stack_page_count: 16,
/// });
/// ```
///
/// Order of the fields does not matter. Every value is configurable.
#[macro_export]
macro_rules! kip_header {
    ($header:ident = $expr:expr) => {
        #[cfg_attr(not(test), link_section = ".kip_header")]
        #[used]
        static $header: $crate::caps::KipHeader = $expr;
    }
}

// TODO: Libuser: capability declaration functions should use type-safe integers.
// BODY: Most of the capability declaration functions use weirdly shaped integers
// BODY: like 12-bits or 21-bits. There is a great crate called [`ux`](https://docs.rs/ux)
// BODY: that provides such weirdly shaped integers. Unfortunately, we cannot
// BODY: create them at compile-time because that would require asserts/panics in
// BODY: const fns.
// BODY:
// BODY: It'd be interesting to revisit this when const fn assertions gets
// BODY: implemented.

/// Create a kernel flag capability. Specifies the lowest/highest priority this
/// process is allowed to take, and which CPUs it is allowed to access.
#[allow(clippy::cast_lossless)] // Can't use From::from in const fn
pub const fn kernel_flags(lowest_prio: u32, highest_prio: u32, lowest_cpuid: u8, highest_cpuid: u8) -> u32 {
    0b111 | ((lowest_prio & 0x3F) << 4) | ((highest_prio & 0x3F) << 10)
        | ((lowest_cpuid as u32) << 16) | ((highest_cpuid as u32) << 24)
}

// TODO: Libuser: implement MapIoOrNormalRange capability.
// BODY: This capability is a bit of a pain. It requires inserting a pair of
// BODY: capabilities inside of the kcap array, so it'll probably require special
// BODY: handling similar to how we handle syscalls.

/// Maps the given physical memory page at a random address on process startup.
pub const fn map_normal_page(page: u32) -> u32 {
    0b1111111 | (page << 8)
}

/// Allows the process to use the given IO Ports directly (through the in/out).
#[allow(clippy::cast_lossless)] // Can't use From::from in const fn
pub const fn ioport(ioport: u16) -> u32 {
   0b1111111111 | ((ioport as u32) << 11)
}

/// Allows the process to create an IRQEvent for those IRQs. Each IRQ should be
/// under or equal to 0xFF, or equal to 0x3FF, in which case the IRQ will be
/// ignored.
#[allow(clippy::cast_lossless)] // Can't use From::from in const fn
pub const fn irq_pair(irq1: u16, irq2: u16) -> u32 {
    0b11111111111 | ((irq1 as u32 & 0x3FF) << 12) | ((irq2 as u32 & 0x3FF) << 22)
}

/// Declare the type of the application. 0 is a sysmodule, 1 is an application,
/// 2 is an applet. Only one application can run at a time.
pub const fn application_type(app_type: u32) -> u32 {
    0b1111111111111 | (app_type & 0b111 << 14)
}

/// The minimum kernel version this process expects.
pub const fn kernel_release_version(version: u32) -> u32 {
    0b11111111111111 | (version << 15)
}

/// Declare the maximum number of live handles this process is allowed to have
/// open.
pub const fn handle_table_size(size: u32) -> u32 {
    0b111111111111111 | ((size & 0x1FF) << 16)
}

/// Declares whether this application can be debugged (e.g. it allows the use
/// of the debug syscalls on it), and whether it can debug other processes.
#[allow(clippy::cast_lossless)] // Can't use From::from in const fn
pub const fn debug_flags(can_be_debugged: bool, can_debug_others: bool) -> u32 {
    0b1111111111111111 | ((can_be_debugged as u32) << 17) | ((can_debug_others as u32) << 18)
}
