//! DMA buffers and MSI, end to end, on QEMU's `edu` teaching device
//! (`1234:11e8`, added by the `boot-test` runs).
//!
//! ```text
//! BAR0 (MMIO)  0x00 id 0x010000ed     0x04 liveness: reads back inverted
//!              0x24 interrupt status  0x60 raise (status |= value)
//!              0x64 acknowledge       0x80 DMA source, 0x88 destination,
//!              0x90 count, 0x98 command (start, to RAM, interrupt 0x100 when done)
//!              its own 4 KiB buffer at device address 0x40000
//! ```
//!
//! One raised interrupt, then a DMA round trip RAM -> device -> RAM, each
//! completion signalled by MSI: exactly three interrupts. A virtual address
//! handed to the device, bus mastering left off or the MSI data at the
//! wrong offset each break this. A watchdog task bounds every wait, so a
//! lost interrupt fails the test instead of hanging it. (A missing EOI
//! still hangs: the vector stays in service and blocks the lower-priority
//! timer, so the watchdog never runs; the runner's timeout catches it.)
//!
//! The runs give edu `dma_mask=0xffffffffffffffff`. Its default 28-bit mask
//! would truncate a direct-map virtual address (`0xffff8000_00000000 +
//! physical`) to exactly the physical one, hiding that bug.

use crate::arch::x86_64::vectors;
use crate::drivers::pci::{self, Enable, MappedBar, device::PciDevice};
use crate::mm::dma::{DmaBuffer, DmaConstraints};
use crate::sync::IrqSpinMutex;
use crate::task::{ExitStatus, WaitQueue};
use crate::{scheduler, serial_println, time};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

const IDENTIFICATION: u64 = 0x00;
const LIVENESS: u64 = 0x04;
const INTERRUPT_STATUS: u64 = 0x24;
const INTERRUPT_RAISE: u64 = 0x60;
const INTERRUPT_ACKNOWLEDGE: u64 = 0x64;
const DMA_SOURCE: u64 = 0x80;
const DMA_DESTINATION: u64 = 0x88;
const DMA_COUNT: u64 = 0x90;
const DMA_COMMAND: u64 = 0x98;
const DMA_START: u64 = 1 << 0;
const DMA_TO_RAM: u64 = 1 << 1;
const DMA_INTERRUPT: u64 = 1 << 2;
/// The interrupt status a finished DMA raises.
const DMA_DONE: u32 = 0x100;
/// Where the device's own buffer is, in its address space.
const DEVICE_BUFFER: u64 = 0x40000;
const LENGTH: usize = 4096;
/// The status our test interrupt carries.
const RAISED: u32 = 0x5a;
/// Pretend edu still had its default 28-bit DMA mask: buffers come from
/// below it, as for a real device that can address only so much.
const EDU_LIMIT: u64 = 1 << 28;
const TIMEOUT_MS: u64 = 3000;

static BASE: AtomicU64 = AtomicU64::new(0);
static INTERRUPTS: AtomicUsize = AtomicUsize::new(0);
static STATUSES: [AtomicU32; 4] = [const { AtomicU32::new(0) }; 4];
static WAIT: IrqSpinMutex<WaitQueue> =
    IrqSpinMutex::new(WaitQueue::new(scheduler::BOOT_TEST_WAIT_ID));
static TIMED_OUT: AtomicBool = AtomicBool::new(false);
static STOP_WATCHDOG: AtomicBool = AtomicBool::new(false);
static WATCHDOG_EXITED: AtomicBool = AtomicBool::new(false);

fn read(offset: u64) -> u32 {
    let address = BASE.load(Ordering::Relaxed) + offset;
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

fn write(offset: u64, value: u32) {
    let address = BASE.load(Ordering::Relaxed) + offset;
    unsafe { core::ptr::write_volatile(address as *mut u32, value) }
}

fn write64(offset: u64, value: u64) {
    let address = BASE.load(Ordering::Relaxed) + offset;
    unsafe { core::ptr::write_volatile(address as *mut u64, value) }
}

/// Interrupt context: acknowledge the device, record why it interrupted,
/// wake the test.
fn interrupt(_vector: u8) {
    let status = read(INTERRUPT_STATUS);
    write(INTERRUPT_ACKNOWLEDGE, status);
    let index = INTERRUPTS.load(Ordering::Relaxed);
    if let Some(slot) = STATUSES.get(index) {
        slot.store(status, Ordering::Relaxed);
    }
    INTERRUPTS.store(index + 1, Ordering::Release);
    scheduler::wake_all(&WAIT);
}

fn watchdog() -> ! {
    let start = time::ticks();
    while !STOP_WATCHDOG.load(Ordering::Acquire) {
        if time::ticks().saturating_sub(start) > TIMEOUT_MS {
            TIMED_OUT.store(true, Ordering::Release);
            scheduler::wake_all(&WAIT);
            break;
        }
        scheduler::sleep_ms(10);
    }
    WATCHDOG_EXITED.store(true, Ordering::Release);
    scheduler::exit(ExitStatus(0))
}

/// Sleep until `count` interrupts have arrived; false on timeout.
fn wait_for(count: usize) -> bool {
    scheduler::wait_event(&WAIT, || {
        INTERRUPTS.load(Ordering::Acquire) >= count || TIMED_OUT.load(Ordering::Acquire)
    });
    INTERRUPTS.load(Ordering::Acquire) >= count
}

fn dma(source: u64, destination: u64, command: u64) {
    write64(DMA_SOURCE, source);
    write64(DMA_DESTINATION, destination);
    write64(DMA_COUNT, LENGTH as u64);
    write64(DMA_COMMAND, DMA_START | DMA_INTERRUPT | command);
}

fn check(name: &str, passed: bool) -> bool {
    if !passed {
        serial_println!("boot-test: dma-msi: {} FAILED", name);
    }
    passed
}

pub fn dma_msi_test() -> bool {
    let Some(edu) = pci::by_id(0x1234, 0x11e8) else {
        return check("edu device present", false);
    };
    pci::enable(
        &edu,
        Enable {
            io: false,
            memory: true,
            bus_master: true,
        },
    );
    let Ok(MappedBar::Memory { address, .. }) = pci::map_bar(&edu, 0) else {
        return check("BAR0 mapped", false);
    };
    BASE.store(address, Ordering::Relaxed);
    let mmio_ok = read(IDENTIFICATION) == 0x0100_00ed && {
        write(LIVENESS, 0x1234_5678);
        read(LIVENESS) == !0x1234_5678
    };
    if !check("MMIO registers", mmio_ok) {
        return false;
    }

    // Mapping the BAR may have built page tables for good; count after.
    let frames_before = super::user::settled_free_frames();
    let (passed, vector) = interrupts_and_dma(&edu);
    let frames_after = super::user::settled_free_frames();

    // The vector is free again (the device no longer sends it).
    let reused = vectors::allocate("boot-test", interrupt);
    let vector_ok = reused.is_some() && reused == vector;
    if let Some(reused) = reused {
        vectors::free(reused);
    }
    let msi_off = !pci::msi_enabled(&edu);
    serial_println!(
        "boot-test: dma-msi interrupts={} vector={:?} statuses={:x?} frames {} -> {}",
        INTERRUPTS.load(Ordering::Acquire),
        vector,
        STATUSES
            .each_ref()
            .map(|status| status.load(Ordering::Relaxed)),
        frames_before,
        frames_after
    );
    passed
        & check("buffers freed", frames_before == frames_after)
        & check("vector reusable", vector_ok)
        & check("MSI off after drop", msi_off)
        & check("no stray interrupts", vectors::stray_count() == 0)
}

/// Everything that holds the MSI and the buffers, so they are dropped on
/// return. Returns the verdict and the vector that was used.
fn interrupts_and_dma(edu: &PciDevice) -> (bool, Option<u8>) {
    let msi = match pci::enable_msi(edu, "edu", interrupt) {
        Ok(msi) => msi,
        Err(error) => {
            serial_println!("boot-test: dma-msi: enable_msi: {:?}", error);
            return (false, None);
        }
    };
    let msi_on = check("MSI on", pci::msi_enabled(edu));
    let listed = check(
        "/proc/interrupts",
        vectors::snapshot()
            .iter()
            .any(|&(vector, owner, _)| vector == msi.vector() && owner == "edu"),
    );
    scheduler::spawn_kernel_task(watchdog, "boot-test-watchdog");

    // 1. An interrupt on request.
    write(INTERRUPT_RAISE, RAISED);
    let raised = check("raised interrupt", wait_for(1));

    // 2. RAM -> device -> RAM.
    let buffers = DmaBuffer::new(LENGTH, DmaConstraints::below(EDU_LIMIT)).and_then(|source| {
        Ok((
            source,
            DmaBuffer::new(LENGTH, DmaConstraints::below(EDU_LIMIT))?,
        ))
    });
    let copied = match buffers {
        Ok((mut source, destination)) => {
            let zeroed = check(
                "fresh buffer zeroed",
                destination.as_slice().iter().all(|&byte| byte == 0),
            );
            let below = check(
                "below the device limit",
                source.device_address().0 + LENGTH as u64 <= EDU_LIMIT,
            );
            for (index, byte) in source.as_mut_slice().iter_mut().enumerate() {
                *byte = (index * 7 + 3) as u8;
            }
            source.sync_for_device();
            dma(source.device_address().0, DEVICE_BUFFER, 0);
            let to_device = check("DMA to device", wait_for(2));
            dma(DEVICE_BUFFER, destination.device_address().0, DMA_TO_RAM);
            let to_ram = check("DMA to RAM", wait_for(3));
            destination.sync_for_cpu();
            zeroed
                & below
                & to_device
                & to_ram
                & check(
                    "data round trip",
                    source.as_slice() == destination.as_slice(),
                )
        }
        Err(error) => {
            serial_println!("boot-test: dma-msi: DmaBuffer::new: {:?}", error);
            false
        }
    };

    // Exactly three, each for the right reason: no storm, none lost.
    scheduler::sleep_ms(20);
    let statuses = STATUSES
        .each_ref()
        .map(|status| status.load(Ordering::Relaxed));
    let exact = check(
        "exactly three interrupts",
        INTERRUPTS.load(Ordering::Acquire) == 3 && statuses[..3] == [RAISED, DMA_DONE, DMA_DONE],
    );

    STOP_WATCHDOG.store(true, Ordering::Release);
    while !WATCHDOG_EXITED.load(Ordering::Acquire) {
        scheduler::sleep_ms(5);
    }
    let vector = msi.vector();
    drop(msi);
    (msi_on & listed & raised & copied & exact, Some(vector))
}
