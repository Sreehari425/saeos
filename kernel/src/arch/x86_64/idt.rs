use crate::arch::x86_64::apic::lapic::SPURIOUS_INTERRUPT_VECTOR;
use crate::arch::x86_64::context;
#[cfg(feature = "keyboard")]
use crate::arch::x86_64::cpu;
use crate::arch::x86_64::gdt;
#[cfg(feature = "keyboard")]
use crate::arch::x86_64::interrupt_controller;
use crate::arch::x86_64::pic;
#[cfg(feature = "keyboard")]
use crate::drivers::keyboard;
use crate::mm::kvm;
use crate::serial_println;

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct InterruptStackFrame {
    pub instruction_pointer: u64,
    pub code_segment: u64,
    pub cpu_flags: u64,
    pub stack_pointer: u64,
    pub stack_segment: u64,
}

#[derive(Clone, Copy)]
#[repr(C, packed)]
pub struct IdtEntry {
    pointer_low: u16,
    gdt_selector: u16,
    options: u16,
    pointer_middle: u16,
    pointer_high: u32,
    reserved: u32,
}

impl IdtEntry {
    pub const fn missing() -> Self {
        Self {
            pointer_low: 0,
            gdt_selector: 0,
            options: 0,
            pointer_middle: 0,
            pointer_high: 0,
            reserved: 0,
        }
    }

    pub fn set_handler_addr(&mut self, addr: u64) -> &mut Self {
        self.pointer_low = addr as u16;
        self.pointer_middle = (addr >> 16) as u16;
        self.pointer_high = (addr >> 32) as u32;
        self.gdt_selector = 0x08; // Kernel Code Segment
        self.options = 0x8E00; // Present, 64-bit Interrupt Gate
        self.reserved = 0;
        self
    }

    pub fn set_stack_index(&mut self, index: u16) -> &mut Self {
        self.options = (self.options & 0xFFF8) | ((index + 1) & 0x07);
        self
    }
}

#[repr(C, packed)]
pub struct InterruptDescriptorTable {
    pub entries: [IdtEntry; 256],
}

impl InterruptDescriptorTable {
    pub const fn new() -> Self {
        Self {
            entries: [IdtEntry::missing(); 256],
        }
    }
}

impl Default for InterruptDescriptorTable {
    fn default() -> Self {
        Self::new()
    }
}

#[repr(C, packed)]
struct IdtDescriptor {
    limit: u16,
    base: u64,
}

static mut IDT: InterruptDescriptorTable = InterruptDescriptorTable::new();

pub fn init() {
    unsafe {
        // 1. CPU Exceptions (vectors 15, 22..=27 and 31 are reserved)
        let exceptions: [(usize, u64); 20] = [
            (0, divide_error_handler as *const () as u64),
            (1, debug_handler as *const () as u64),
            (3, breakpoint_handler as *const () as u64),
            (4, overflow_handler as *const () as u64),
            (5, bound_range_handler as *const () as u64),
            (6, invalid_opcode_handler as *const () as u64),
            (7, device_not_available_handler as *const () as u64),
            (9, coprocessor_overrun_handler as *const () as u64),
            (10, invalid_tss_handler as *const () as u64),
            (11, segment_not_present_handler as *const () as u64),
            (12, stack_segment_handler as *const () as u64),
            (13, general_protection_fault_handler as *const () as u64),
            (14, context::PAGE_FAULT_ENTRY as *const () as u64),
            (16, x87_floating_point_handler as *const () as u64),
            (17, alignment_check_handler as *const () as u64),
            (19, simd_floating_point_handler as *const () as u64),
            (20, virtualization_handler as *const () as u64),
            (21, control_protection_handler as *const () as u64),
            (28, hypervisor_injection_handler as *const () as u64),
            (29, vmm_communication_handler as *const () as u64),
        ];
        for (vector, handler) in exceptions {
            IDT.entries[vector].set_handler_addr(handler);
        }
        IDT.entries[30].set_handler_addr(security_handler as *const () as u64);
        // #DF, NMI, and #MC can arrive with the current stack unusable (an
        // overflow, or at any instruction), so each has its own IST stack.
        IDT.entries[8]
            .set_handler_addr(double_fault_handler as *const () as u64)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        IDT.entries[2]
            .set_handler_addr(nmi_handler as *const () as u64)
            .set_stack_index(gdt::NMI_IST_INDEX);
        IDT.entries[18]
            .set_handler_addr(machine_check_handler as *const () as u64)
            .set_stack_index(gdt::MACHINE_CHECK_IST_INDEX);
        // #GP and #PF run on the current stack. Sharing the #DF IST would let
        // a nested fault overwrite the outer frame, and a page-fault handler
        // that may one day block cannot live on a per-CPU IST stack. A fault
        // that cannot push its frame (a kernel stack overflow into a guard
        // page) escalates to #DF, which keeps its own IST stack.

        // 2. Hardware Interrupts (Vectors 0x20 = 32, 0x21 = 33)
        IDT.entries[pic::PIC_1_OFFSET as usize]
            .set_handler_addr(context::TIMER_INTERRUPT_ENTRY as *const () as u64);
        #[cfg(feature = "keyboard")]
        IDT.entries[(pic::PIC_1_OFFSET + 1) as usize]
            .set_handler_addr(keyboard_interrupt_handler as *const () as u64);
        #[cfg(feature = "serial")]
        IDT.entries[crate::drivers::serial::RECEIVE_VECTOR as usize]
            .set_handler_addr(serial_interrupt_handler as *const () as u64);

        // 3. APIC Spurious Interrupt Handler (Vector 0xFF = 255)
        IDT.entries[SPURIOUS_INTERRUPT_VECTOR as usize]
            .set_handler_addr(spurious_interrupt_handler as *const () as u64);

        let idt_descriptor = IdtDescriptor {
            limit: (core::mem::size_of::<InterruptDescriptorTable>() - 1) as u16,
            base: (&raw const IDT) as u64,
        };

        core::arch::asm!(
            "lidt [{ptr}]",
            ptr = in(reg) &idt_descriptor,
            options(readonly, nostack, preserves_flags)
        );
    }
}

// Exception Handlers
//
// Every exception except #BP is fatal for now and reports through
// `crate::fatal`, which cannot deadlock on a lock held by the faulting code.
// Once user mode exists, faults from ring 3 will kill the process instead.

/// Formats an optional exception error code.
struct ErrorCode(Option<u64>);

impl core::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(code) => write!(f, " err={code:#x}"),
            None => Ok(()),
        }
    }
}

/// The running task, if the scheduler lock is free (the fault may have
/// happened while it was held).
struct CurrentTask;

impl core::fmt::Display for CurrentTask {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let task = crate::scheduler::SCHEDULER
            .try_lock()
            .and_then(|scheduler| {
                let id = scheduler.current()?;
                scheduler.task(id).map(|task| (id, task.name))
            });
        match task {
            Some((id, name)) => write!(f, "task '{}' (id {})", name, id.0),
            None => f.write_str("task unknown"),
        }
    }
}

/// Vector a `fault-test` build deliberately triggers (`ud2`).
const FAULT_TEST_VECTOR: u8 = 6;

fn report_exception(
    vector: u8,
    name: &str,
    error_code: Option<u64>,
    frame: &InterruptStackFrame,
) -> ! {
    let expected = cfg!(feature = "fault-test") && vector == FAULT_TEST_VECTOR;
    crate::fatal::report(
        format_args!(
            "[EXCEPTION {}: {}]{} RIP={:#x} CS={:#x} RFLAGS={:#x} RSP={:#x} {}",
            vector,
            name,
            ErrorCode(error_code),
            frame.instruction_pointer,
            frame.code_segment,
            frame.cpu_flags,
            frame.stack_pointer,
            CurrentTask
        ),
        expected,
    )
}

/// A fault in user mode kills the process with the matching signal, as on
/// Linux; the kernel carries on. Never returns.
fn kill_user(name: &str, signal: i32, instruction_pointer: u64, address: Option<u64>) -> ! {
    let (pid, task) = crate::scheduler::with_scheduler(|scheduler| {
        scheduler
            .current()
            .and_then(|task| scheduler.task(task))
            .map_or((0, "?"), |task| {
                (task.process_id.map_or(0, |pid| pid.0), task.name)
            })
    });
    match address {
        Some(address) => crate::kerror!(
            "[pid {} '{}'] killed by {}: {} at {:#x}, rip={:#x}",
            pid,
            task,
            crate::init::signal_name(signal),
            name,
            address,
            instruction_pointer
        ),
        None => crate::kerror!(
            "[pid {} '{}'] killed by {}: {}, rip={:#x}",
            pid,
            task,
            crate::init::signal_name(signal),
            name,
            instruction_pointer
        ),
    }
    crate::scheduler::exit_current(crate::task::ExitStatus::killed(signal))
}

const SIGILL: i32 = 4;
const SIGTRAP: i32 = 5;
const SIGBUS: i32 = 7;
const SIGFPE: i32 = 8;
const SIGSEGV: i32 = 11;

/// The signal a user-mode exception delivers (Linux's mapping).
const fn signal_for(vector: u8) -> i32 {
    match vector {
        0 | 16 | 19 => SIGFPE,
        1 | 3 => SIGTRAP,
        6 => SIGILL,
        7 => SIGFPE,
        11 | 12 | 17 => SIGBUS,
        _ => SIGSEGV,
    }
}

macro_rules! fatal_exception {
    ($handler:ident, $vector:expr, $name:expr) => {
        extern "x86-interrupt" fn $handler(frame: InterruptStackFrame) {
            if crate::arch::x86_64::syscall::from_user(frame.code_segment) {
                kill_user($name, signal_for($vector), frame.instruction_pointer, None);
            }
            report_exception($vector, $name, None, &frame)
        }
    };
    ($handler:ident, $vector:expr, $name:expr, error_code) => {
        extern "x86-interrupt" fn $handler(frame: InterruptStackFrame, error_code: u64) {
            if crate::arch::x86_64::syscall::from_user(frame.code_segment) {
                kill_user($name, signal_for($vector), frame.instruction_pointer, None);
            }
            report_exception($vector, $name, Some(error_code), &frame)
        }
    };
}

fatal_exception!(divide_error_handler, 0, "DIVIDE ERROR (#DE)");
fatal_exception!(debug_handler, 1, "DEBUG (#DB)");
fatal_exception!(nmi_handler, 2, "NON-MASKABLE INTERRUPT (NMI)");
fatal_exception!(overflow_handler, 4, "OVERFLOW (#OF)");
fatal_exception!(bound_range_handler, 5, "BOUND RANGE EXCEEDED (#BR)");
fatal_exception!(invalid_opcode_handler, 6, "INVALID OPCODE (#UD)");
fatal_exception!(
    device_not_available_handler,
    7,
    "DEVICE NOT AVAILABLE (#NM)"
);
fatal_exception!(
    coprocessor_overrun_handler,
    9,
    "COPROCESSOR SEGMENT OVERRUN"
);
fatal_exception!(invalid_tss_handler, 10, "INVALID TSS (#TS)", error_code);
fatal_exception!(
    segment_not_present_handler,
    11,
    "SEGMENT NOT PRESENT (#NP)",
    error_code
);
fatal_exception!(
    stack_segment_handler,
    12,
    "STACK-SEGMENT FAULT (#SS)",
    error_code
);
fatal_exception!(
    general_protection_fault_handler,
    13,
    "GENERAL PROTECTION FAULT (#GP)",
    error_code
);
fatal_exception!(x87_floating_point_handler, 16, "x87 FLOATING-POINT (#MF)");
fatal_exception!(
    alignment_check_handler,
    17,
    "ALIGNMENT CHECK (#AC)",
    error_code
);
fatal_exception!(machine_check_handler, 18, "MACHINE CHECK (#MC)");
fatal_exception!(simd_floating_point_handler, 19, "SIMD FLOATING-POINT (#XM)");
fatal_exception!(virtualization_handler, 20, "VIRTUALIZATION (#VE)");
fatal_exception!(
    control_protection_handler,
    21,
    "CONTROL PROTECTION (#CP)",
    error_code
);
fatal_exception!(
    hypervisor_injection_handler,
    28,
    "HYPERVISOR INJECTION (#HV)"
);
fatal_exception!(
    vmm_communication_handler,
    29,
    "VMM COMMUNICATION (#VC)",
    error_code
);
fatal_exception!(security_handler, 30, "SECURITY (#SX)", error_code);

/// `int3` is a trap: RIP already points past it, so returning resumes the
/// program. The linker pads between functions with 0xCC; landing in that
/// padding (another 0xCC at RIP) means a wild jump, which stays fatal.
extern "x86-interrupt" fn breakpoint_handler(frame: InterruptStackFrame) {
    let next = unsafe { core::ptr::read_volatile(frame.instruction_pointer as *const u8) };
    if next == 0xcc {
        report_exception(3, "BREAKPOINT IN INT3 PADDING (wild jump?)", None, &frame);
    }
    BREAKPOINTS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    serial_println!(
        "[BREAKPOINT] RIP={:#x} RSP={:#x}; resuming",
        frame.instruction_pointer - 1,
        frame.stack_pointer
    );
}

static BREAKPOINTS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Number of `int3` breakpoints handled and resumed.
pub fn breakpoints_handled() -> u64 {
    BREAKPOINTS.load(core::sync::atomic::Ordering::Relaxed)
}

fn read_cr2() -> u64 {
    let address: u64;
    unsafe {
        core::arch::asm!("mov {}, cr2", out(reg) address, options(nomem, nostack, preserves_flags));
    }
    address
}

/// If `address` hit a kernel-stack guard, report the overflowing task and
/// stop. An overflow is the expected outcome only in the guard test.
fn check_stack_overflow(address: u64, instruction_pointer: u64) {
    let Some(kvm::FaultKind::StackGuard { slot, stack }) = kvm::classify_fault(address) else {
        return;
    };
    let passed = cfg!(feature = "mm-guard-test");
    match crate::scheduler::try_task_on_stack(stack.base) {
        Some((task, name)) => crate::fatal::report(
            format_args!(
                "[KERNEL STACK OVERFLOW in task '{}' (id {}), stack {:#x}..{:#x}, fault {:#x}, RIP={:#x}]",
                name, task.0, stack.base, stack.top, address, instruction_pointer
            ),
            passed,
        ),
        None => crate::fatal::report(
            format_args!(
                "[KERNEL STACK OVERFLOW in stack slot {}, stack {:#x}..{:#x}, fault {:#x}, RIP={:#x}]",
                slot, stack.base, stack.top, address, instruction_pointer
            ),
            passed,
        ),
    }
}

extern "x86-interrupt" fn double_fault_handler(frame: InterruptStackFrame, error_code: u64) -> ! {
    // A guard-page hit cannot push a #PF frame, so overflows arrive here with
    // CR2 still naming the guard address.
    check_stack_overflow(read_cr2(), frame.instruction_pointer);
    report_exception(8, "DOUBLE FAULT (#DF)", Some(error_code), &frame)
}

/// Registers saved by `saeos_page_fault_entry`, then the CPU's frame.
#[repr(C)]
pub struct PageFaultFrame {
    pub registers: [u64; 15],
    pub error_code: u64,
    pub instruction_pointer: u64,
    pub code_segment: u64,
    pub cpu_flags: u64,
    pub stack_pointer: u64,
    pub stack_segment: u64,
}

/// Called by `saeos_page_fault_entry` with interrupts disabled. A fault in
/// a user-memory copy routine resumes at its fixup (the copy returns
/// EFAULT); anything else is a kernel bug and is reported as before.
#[unsafe(no_mangle)]
extern "sysv64" fn saeos_page_fault_rust(frame: &mut PageFaultFrame) {
    let address = read_cr2();
    // A write to a present page in the user half may be copy-on-write
    // (after fork), from user code or from a kernel copy_to_user.
    const PRESENT_AND_WRITE: u64 = 0b11;
    if frame.error_code & PRESENT_AND_WRITE == PRESENT_AND_WRITE
        && address < crate::mm::uaccess::USER_TOP
    {
        match crate::mm::address_space::resolve_cow(address) {
            crate::mm::paging::CowResolution::Resolved => return,
            crate::mm::paging::CowResolution::OutOfMemory
                if crate::arch::x86_64::syscall::from_user(frame.code_segment) =>
            {
                kill_user(
                    "OUT OF MEMORY (copy-on-write)",
                    SIGBUS,
                    frame.instruction_pointer,
                    Some(address),
                );
            }
            _ => {}
        }
    }
    if let Some(fixup) = crate::mm::uaccess::fixup_for(frame.instruction_pointer, address) {
        frame.instruction_pointer = fixup;
        return;
    }
    if crate::arch::x86_64::syscall::from_user(frame.code_segment) {
        kill_user(
            "PAGE FAULT",
            SIGSEGV,
            frame.instruction_pointer,
            Some(address),
        );
    }
    // A large frame can touch the guard while RSP is still on the stack, in
    // which case the #PF itself is delivered normally.
    check_stack_overflow(address, frame.instruction_pointer);
    crate::fatal::report(
        format_args!(
            "[EXCEPTION 14: PAGE FAULT (#PF)] address={:#x} err={:#x} ({}) RIP={:#x} RSP={:#x} {}",
            address,
            frame.error_code,
            PageFaultCause(frame.error_code),
            frame.instruction_pointer,
            frame.stack_pointer,
            CurrentTask
        ),
        false,
    )
}

/// Decodes the #PF error code bits.
struct PageFaultCause(u64);

impl core::fmt::Display for PageFaultCause {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let code = self.0;
        f.write_str(if code & 1 != 0 {
            "protection"
        } else {
            "not present"
        })?;
        f.write_str(if code & 2 != 0 { ", write" } else { ", read" })?;
        if code & 4 != 0 {
            f.write_str(", user")?;
        }
        if code & 8 != 0 {
            f.write_str(", reserved bit")?;
        }
        if code & 16 != 0 {
            f.write_str(", instruction fetch")?;
        }
        Ok(())
    }
}

// Hardware Interrupt Handlers
#[cfg(feature = "keyboard")]
extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let _irq = crate::arch::x86_64::irq::enter();
    let scancode = unsafe { cpu::inb(0x60) };

    keyboard::handle_scancode(scancode);

    interrupt_controller::notify_end_of_interrupt(pic::PIC_1_OFFSET + 1);
}

/// COM1 received data: queue it as console input.
#[cfg(feature = "serial")]
extern "x86-interrupt" fn serial_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let _irq = crate::arch::x86_64::irq::enter();
    crate::drivers::serial::drain_input();
    crate::arch::x86_64::interrupt_controller::notify_end_of_interrupt(
        crate::drivers::serial::RECEIVE_VECTOR,
    );
}

// APIC Spurious Interrupt Handler (Intel specification states no EOI should be generated)
extern "x86-interrupt" fn spurious_interrupt_handler(_stack_frame: InterruptStackFrame) {}
