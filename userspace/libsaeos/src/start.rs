//! Program entry and the panic handler.
//!
//! ```text
//! kernel exec -> _start (rsp = argc, argv..., NULL, envp..., NULL, auxv...)
//!   -> start: save argv/envp, set up thread-local storage
//!   -> saeos_main(Args) (the program's `main`, via `entry!`)
//!   -> exit(code): flush stdout, end the process
//! ```

use crate::startinfo::StartInfo;

/// The program's arguments, from the initial stack (see also
/// [`crate::env::args`]).
#[derive(Clone, Copy)]
pub struct Args {
    count: usize,
    vector: *const *const u8,
}

impl Args {
    pub fn count(&self) -> usize {
        self.count
    }

    /// Argument `index` without its NUL.
    pub fn get(&self, index: usize) -> Option<&'static [u8]> {
        if index >= self.count {
            return None;
        }
        Some(unsafe { crate::startinfo::c_string(crate::startinfo::entry(self.vector, index)) })
    }
}

unsafe extern "Rust" {
    /// Defined by [`crate::entry!`].
    fn saeos_main(args: Args) -> i32;
}

/// Declare the program's `main(Args) -> i32`.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[unsafe(no_mangle)]
        fn saeos_main(args: $crate::Args) -> i32 {
            $main(args)
        }
    };
}

/// The ELF entry point. The kernel starts us with rsp at argc and nothing
/// else set up; align the stack as the SysV ABI requires for a call.
///
/// # Safety
/// Only the kernel may enter here, with the initial stack `exec` builds.
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "mov rdi, rsp",
        "and rsp, -16",
        "xor ebp, ebp",
        "call {start}",
        "ud2",
        start = sym start,
    )
}

unsafe extern "C" fn start(stack: *const u64) -> ! {
    let info = unsafe { StartInfo::from_stack(stack) };
    // Thread-local storage first: errno lives there.
    crate::tls::init(&info);
    crate::env::init(info);
    let args = Args {
        count: info.argc,
        vector: info.argv,
    };
    crate::process::exit(unsafe { saeos_main(args) })
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    crate::eprintln!("panic: {}", info);
    crate::raw::exit_now(101)
}
