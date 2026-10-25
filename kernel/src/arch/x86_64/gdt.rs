use core::mem::size_of;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
pub const NMI_IST_INDEX: u16 = 1;
pub const MACHINE_CHECK_IST_INDEX: u16 = 2;

#[repr(C, packed)]
pub struct TaskStateSegment {
    reserved_1: u32,
    pub privilege_stack_table: [u64; 3],
    reserved_2: u64,
    pub interrupt_stack_table: [u64; 7],
    reserved_3: u64,
    reserved_4: u16,
    pub iomap_base: u16,
}

impl TaskStateSegment {
    pub const fn new() -> Self {
        Self {
            reserved_1: 0,
            privilege_stack_table: [0; 3],
            reserved_2: 0,
            interrupt_stack_table: [0; 7],
            reserved_3: 0,
            reserved_4: 0,
            iomap_base: size_of::<Self>() as u16,
        }
    }
}

impl Default for TaskStateSegment {
    fn default() -> Self {
        Self::new()
    }
}

// 16 KB IST stacks for exceptions that may arrive on an unusable stack.
const STACK_SIZE: usize = 4096 * 4;
#[repr(C, align(16))]
struct IstStack([u8; STACK_SIZE]);
static mut DOUBLE_FAULT_STACK: IstStack = IstStack([0; STACK_SIZE]);
static mut NMI_STACK: IstStack = IstStack([0; STACK_SIZE]);
static mut MACHINE_CHECK_STACK: IstStack = IstStack([0; STACK_SIZE]);

static mut TSS: TaskStateSegment = TaskStateSegment::new();

#[repr(C, packed)]
struct GdtDescriptor {
    limit: u16,
    base: u64,
}

/// Segment selectors. The user pair sits directly above the kernel pair in
/// the order `SYSRET` requires: it loads SS from `STAR[63:48] + 8` and CS
/// from `STAR[63:48] + 16`, so `STAR[63:48] = KERNEL_DATA` will select
/// `USER_DATA` and `USER_CODE` (with RPL 3).
pub const KERNEL_CODE: u16 = 0x08;
pub const KERNEL_DATA: u16 = 0x10;
pub const USER_DATA: u16 = 0x18 | 3;
pub const USER_CODE: u16 = 0x20 | 3;
pub const TSS_SELECTOR: u16 = 0x28;

const GDT_ENTRIES: usize = 7;
/// Null, kernel code, kernel data, user data, user code, TSS (two slots).
static mut GDT: [u64; GDT_ENTRIES] = [0; GDT_ENTRIES];

/// The descriptors `init` installs, for the boot test to compare against.
pub const KERNEL_CODE_DESCRIPTOR: u64 = 0x0020_9A00_0000_0000;
pub const KERNEL_DATA_DESCRIPTOR: u64 = 0x0000_9200_0000_0000;
/// Present, DPL 3, writable data.
pub const USER_DATA_DESCRIPTOR: u64 = 0x0000_F200_0000_0000;
/// Present, DPL 3, executable, long mode.
pub const USER_CODE_DESCRIPTOR: u64 = 0x0020_FA00_0000_0000;

/// Kernel stack the CPU switches to when an interrupt or exception arrives
/// in user mode. Set on every task switch to the incoming task's stack top.
pub fn set_kernel_stack(top: u64) {
    unsafe {
        (&raw mut TSS.privilege_stack_table)
            .cast::<u64>()
            .write_unaligned(top);
    }
}

pub fn kernel_stack() -> u64 {
    unsafe {
        (&raw const TSS.privilege_stack_table)
            .cast::<u64>()
            .read_unaligned()
    }
}

/// The live GDT entries and task register, read back from the CPU.
pub fn installed() -> ([u64; GDT_ENTRIES], u16) {
    let mut descriptor = GdtDescriptor { limit: 0, base: 0 };
    let task_register: u16;
    unsafe {
        core::arch::asm!("sgdt [{}]", in(reg) &raw mut descriptor, options(nostack, preserves_flags));
        core::arch::asm!("str {0:x}", out(reg) task_register, options(nomem, nostack, preserves_flags));
    }
    let base = descriptor.base;
    let mut entries = [0u64; GDT_ENTRIES];
    if usize::from(descriptor.limit) + 1 == size_of::<[u64; GDT_ENTRIES]>() {
        for (index, entry) in entries.iter_mut().enumerate() {
            *entry = unsafe { (base as *const u64).add(index).read() };
        }
    }
    (entries, task_register)
}

pub fn init() {
    unsafe {
        // Interrupt Stack Table entries (the CPU loads RSP from these).
        for (index, stack) in [
            (DOUBLE_FAULT_IST_INDEX, &raw const DOUBLE_FAULT_STACK),
            (NMI_IST_INDEX, &raw const NMI_STACK),
            (MACHINE_CHECK_IST_INDEX, &raw const MACHINE_CHECK_STACK),
        ] {
            TSS.interrupt_stack_table[index as usize] = stack as u64 + STACK_SIZE as u64;
        }

        // Entry 0: Null descriptor
        GDT[0] = 0;

        GDT[1] = KERNEL_CODE_DESCRIPTOR;
        GDT[2] = KERNEL_DATA_DESCRIPTOR;
        GDT[3] = USER_DATA_DESCRIPTOR;
        GDT[4] = USER_CODE_DESCRIPTOR;

        // Entries 5 & 6: TSS descriptor (16 bytes in long mode).
        let tss_ptr = (&raw const TSS) as u64;
        let tss_limit = (size_of::<TaskStateSegment>() - 1) as u64;

        let mut tss_low: u64 = tss_limit & 0xFFFF; // Bytes 0..1: Limit 0..15
        tss_low |= (tss_ptr & 0x00FF_FFFF) << 16; // Bytes 2..4: Base 0..23
        tss_low |= 0x0000_8900_0000_0000; // Byte 5: Present (1), Ring 0 (00), Type 0x9 (64-bit TSS)
        tss_low |= ((tss_limit >> 16) & 0x0F) << 48; // Byte 6: Limit 16..19
        tss_low |= ((tss_ptr >> 24) & 0xFF) << 56; // Byte 7: Base 24..31
        GDT[5] = tss_low;
        GDT[6] = tss_ptr >> 32;

        let gdt_descriptor = GdtDescriptor {
            limit: (size_of::<[u64; GDT_ENTRIES]>() - 1) as u16,
            base: (&raw const GDT) as *const u64 as u64,
        };

        // Load GDT
        core::arch::asm!(
            "lgdt [{ptr}]",
            ptr = in(reg) &gdt_descriptor,
            options(readonly, nostack, preserves_flags)
        );

        // Reload Code Segment (CS) to 0x08 and Data Segments to 0x10
        core::arch::asm!(
            "push {code_sel}",
            "lea {tmp}, [2f + rip]",
            "push {tmp}",
            "retfq",
            "2:",
            "mov ds, {data_sel:x}",
            "mov es, {data_sel:x}",
            "mov ss, {data_sel:x}",
            "mov fs, {data_sel:x}",
            "mov gs, {data_sel:x}",
            code_sel = in(reg) u64::from(KERNEL_CODE),
            data_sel = in(reg) KERNEL_DATA,
            tmp = out(reg) _,
        );

        // Load Task State Segment (TSS) into Task Register (TR)
        core::arch::asm!(
            "ltr {tss_sel:x}",
            tss_sel = in(reg) TSS_SELECTOR,
            options(nomem, nostack, preserves_flags)
        );
    }
}
