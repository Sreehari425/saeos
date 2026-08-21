use core::mem::size_of;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

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

// 16 KB stack for Double Fault exception
const STACK_SIZE: usize = 4096 * 4;
static mut DOUBLE_FAULT_STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

static mut TSS: TaskStateSegment = TaskStateSegment::new();

#[repr(C, packed)]
struct GdtDescriptor {
    limit: u16,
    base: u64,
}

// 64-bit GDT: Null (0), Code (0x08), Data (0x10), TSS (0x18, takes 2 slots = 16 bytes)
static mut GDT: [u64; 5] = [0; 5];

pub fn init() {
    unsafe {
        // Set up TSS Interrupt Stack Table entry 0 for Double Faults
        let stack_start = (&raw const DOUBLE_FAULT_STACK) as *const u8 as u64;
        let stack_end = stack_start + STACK_SIZE as u64;
        TSS.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = stack_end;

        // Entry 0: Null descriptor
        GDT[0] = 0;

        // Entry 1: 64-bit Kernel Code segment (Selector 0x08)
        // Executable (bit 43), Code/Data (bit 44), Present (bit 47), 64-bit Long mode (bit 53)
        GDT[1] = 0x00209A0000000000;

        // Entry 2: 64-bit Kernel Data segment (Selector 0x10)
        // Writable (bit 41), Code/Data (bit 44), Present (bit 47)
        GDT[2] = 0x0000920000000000;

        // Entries 3 & 4: TSS Descriptor (16 bytes in 64-bit mode, Selector 0x18)
        let tss_ptr = (&raw const TSS) as u64;
        let tss_limit = (size_of::<TaskStateSegment>() - 1) as u64;

        let mut tss_low: u64 = tss_limit & 0xFFFF; // Bytes 0..1: Limit 0..15
        tss_low |= (tss_ptr & 0x00FF_FFFF) << 16;  // Bytes 2..4: Base 0..23
        tss_low |= 0x0000_8900_0000_0000;          // Byte 5: Present (1), Ring 0 (00), Type 0x9 (64-bit TSS)
        tss_low |= ((tss_limit >> 16) & 0x0F) << 48; // Byte 6: Limit 16..19
        tss_low |= ((tss_ptr >> 24) & 0xFF) << 56;   // Byte 7: Base 24..31
        GDT[3] = tss_low;

        // High 8 bytes of TSS descriptor: Base 32..63
        GDT[4] = tss_ptr >> 32;

        let gdt_descriptor = GdtDescriptor {
            limit: (size_of::<[u64; 5]>() - 1) as u16,
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
            code_sel = in(reg) 0x08u64,
            data_sel = in(reg) 0x10u16,
            tmp = out(reg) _,
        );

        // Load Task State Segment (TSS) into Task Register (TR)
        core::arch::asm!(
            "ltr {tss_sel:x}",
            tss_sel = in(reg) 0x18u16,
            options(nomem, nostack, preserves_flags)
        );
    }
}
