//! What the kernel hands a new program: the initial stack (SysV x86_64).
//!
//! ```text
//! rsp -> argc, argv[0..argc], NULL, envp[...], NULL, auxv (type, value)..., AT_NULL
//! ```
//!
//! Pure pointer walking, tested in the host model against the kernel's
//! own stack builder (`kernel/src/exec/stack.rs`).

pub const AT_NULL: u64 = 0;
pub const AT_PHDR: u64 = 3;
pub const AT_PHNUM: u64 = 5;
pub const AT_PAGESZ: u64 = 6;
pub const AT_ENTRY: u64 = 9;
pub const AT_RANDOM: u64 = 25;

#[derive(Debug, Clone, Copy)]
pub struct StartInfo {
    pub argc: usize,
    pub argv: *const *const u8,
    pub envp: *const *const u8,
    pub auxv: *const [u64; 2],
}

impl StartInfo {
    /// # Safety
    /// `stack` must be the initial stack pointer the kernel provided.
    pub unsafe fn from_stack(stack: *const u64) -> Self {
        unsafe {
            let argc = *stack as usize;
            let argv = stack.add(1) as *const *const u8;
            let envp = argv.add(argc + 1);
            let mut count = 0;
            while !entry(envp, count).is_null() {
                count += 1;
            }
            let auxv = envp.add(count + 1) as *const [u64; 2];
            Self {
                argc,
                argv,
                envp,
                auxv,
            }
        }
    }

    /// The value of auxiliary vector entry `kind`.
    pub fn aux(&self, kind: u64) -> Option<u64> {
        let mut entry = self.auxv;
        loop {
            let [entry_kind, value] = unsafe { *entry };
            if entry_kind == AT_NULL {
                return None;
            }
            if entry_kind == kind {
                return Some(value);
            }
            entry = unsafe { entry.add(1) };
        }
    }
}

/// Entry `index` of a pointer array the kernel wrote (argv, envp). The
/// kernel stores plain addresses, so this reads an address and takes its
/// exposed provenance rather than loading a pointer from integer bytes.
///
/// # Safety
/// `array` must have at least `index + 1` entries.
pub unsafe fn entry(array: *const *const u8, index: usize) -> *const u8 {
    core::ptr::with_exposed_provenance(unsafe { (array as *const usize).add(index).read() })
}

/// A NUL-terminated string from the stack.
///
/// # Safety
/// `pointer` must point at a NUL-terminated string that lives forever (the
/// initial stack's strings do).
pub unsafe fn c_string(pointer: *const u8) -> &'static [u8] {
    unsafe {
        let mut length = 0;
        while *pointer.add(length) != 0 {
            length += 1;
        }
        core::slice::from_raw_parts(pointer, length)
    }
}
