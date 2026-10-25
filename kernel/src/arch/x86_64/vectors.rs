//! Interrupt vectors handed out at run time, for MSI.
//!
//! ```text
//! 0x00..0x20  CPU exceptions
//! 0x20..0x30  legacy IRQs (timer, keyboard, COM1, ...) at the PIC offsets
//! 0x40..0x60  this allocator: one per MSI user          <- FIRST..FIRST+COUNT
//! 0xff        APIC spurious
//! ```
//!
//! Each vector has its own IDT stub. It marks interrupt context, calls the
//! registered handler with the vector, and sends the Local APIC EOI: MSI
//! goes straight to the Local APIC, never through the 8259 PIC. Handlers
//! are read with one atomic load, so the interrupt path takes no lock.
//! An interrupt on a vector nobody owns is counted as stray.

use super::idt::InterruptStackFrame;
use crate::sync::IrqSpinMutex;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub const FIRST: u8 = 0x40;
pub const COUNT: usize = 32;

/// An interrupt handler; it gets the vector it was registered for.
pub type Handler = fn(u8);

/// The handler of each vector, as a function address (0: none).
static HANDLERS: [AtomicUsize; COUNT] = [const { AtomicUsize::new(0) }; COUNT];
static COUNTS: [AtomicU64; COUNT] = [const { AtomicU64::new(0) }; COUNT];
static STRAY: AtomicU64 = AtomicU64::new(0);
/// Who owns each vector; also what the allocator allocates from.
static OWNERS: IrqSpinMutex<[Option<&'static str>; COUNT]> = IrqSpinMutex::new([None; COUNT]);

/// A free vector for `handler`, or `None` if all are taken.
pub fn allocate(owner: &'static str, handler: Handler) -> Option<u8> {
    let mut owners = OWNERS.lock();
    let index = owners.iter().position(Option::is_none)?;
    owners[index] = Some(owner);
    COUNTS[index].store(0, Ordering::Relaxed);
    HANDLERS[index].store(handler as usize, Ordering::Release);
    Some(FIRST + index as u8)
}

/// Give a vector back. The device must no longer send it.
pub fn free(vector: u8) {
    let index = index_of(vector).expect("freeing a vector the allocator does not own");
    let mut owners = OWNERS.lock();
    assert!(owners[index].is_some(), "vector {vector:#x} freed twice");
    HANDLERS[index].store(0, Ordering::Release);
    owners[index] = None;
}

fn index_of(vector: u8) -> Option<usize> {
    let index = usize::from(vector.checked_sub(FIRST)?);
    (index < COUNT).then_some(index)
}

/// Interrupts on unowned vectors so far.
pub fn stray_count() -> u64 {
    STRAY.load(Ordering::Relaxed)
}

/// `(vector, owner, interrupts)` for each allocated vector.
pub fn snapshot() -> alloc::vec::Vec<(u8, &'static str, u64)> {
    let owners = *OWNERS.lock();
    owners
        .iter()
        .enumerate()
        .filter_map(|(index, owner)| {
            owner.map(|owner| {
                (
                    FIRST + index as u8,
                    owner,
                    COUNTS[index].load(Ordering::Relaxed),
                )
            })
        })
        .collect()
}

/// `/proc/interrupts`: `vector: count owner` per allocated vector, then
/// the stray count.
pub fn proc_text() -> alloc::vec::Vec<u8> {
    use core::fmt::Write;
    let mut text = alloc::string::String::new();
    for (vector, owner, count) in snapshot() {
        let _ = writeln!(text, "{vector:#04x}: {count:>10} {owner}");
    }
    let _ = writeln!(text, "stray: {}", stray_count());
    text.into_bytes()
}

fn dispatch(index: usize) {
    let _irq = super::irq::enter();
    let vector = FIRST + index as u8;
    match HANDLERS[index].load(Ordering::Acquire) {
        0 => {
            STRAY.fetch_add(1, Ordering::Relaxed);
        }
        address => {
            COUNTS[index].fetch_add(1, Ordering::Relaxed);
            let handler: Handler = unsafe { core::mem::transmute::<usize, Handler>(address) };
            handler(vector);
        }
    }
    super::interrupt_controller::notify_end_of_interrupt(vector);
}

macro_rules! stubs {
    ($($index:literal)*) => {
        /// The IDT entry for each vector, in order.
        pub static STUBS: [extern "x86-interrupt" fn(InterruptStackFrame); COUNT] = [$({
            extern "x86-interrupt" fn stub(_frame: InterruptStackFrame) {
                dispatch($index);
            }
            stub
        }),*];
    };
}

stubs!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31);
