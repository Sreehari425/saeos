use crate::apic::{IoApic, LocalApic};
use crate::pic;
use crate::sync::SpinMutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerKind {
    Apic,
    LegacyPic,
}

pub struct InterruptController {
    pub lapic: LocalApic,
    pub ioapic: IoApic,
    pub mode: ControllerKind,
}

impl InterruptController {
    pub const fn new() -> Self {
        Self {
            lapic: LocalApic::new(),
            ioapic: IoApic::new(),
            mode: ControllerKind::LegacyPic,
        }
    }

    pub fn init(&mut self) -> ControllerKind {
        // 1. Remap legacy PIC offsets first so no vectors collide with CPU exceptions (0..31)
        pic::init();

        // 2. Attempt to initialize Local APIC
        if self.lapic.init().is_ok() {
            let bsp_id = self.lapic.id() as u8;

            // 3. Attempt to initialize I/O APIC routing to BSP
            if self.ioapic.init(bsp_id).is_ok() {
                // Completely mask 8259 PIC to disable legacy IRQ routing
                unsafe {
                    core::arch::asm!("out 0x21, al", in("al") 0xFFu8, options(nomem, nostack, preserves_flags));
                    core::arch::asm!("out 0xa1, al", in("al") 0xFFu8, options(nomem, nostack, preserves_flags));
                }

                self.mode = ControllerKind::Apic;
                return ControllerKind::Apic;
            }
        }

        // 4. Fallback Path: keep 8259 PIC active with Timer & Keyboard unmasked
        self.mode = ControllerKind::LegacyPic;
        ControllerKind::LegacyPic
    }

    pub fn notify_end_of_interrupt(&self, vector: u8) {
        match self.mode {
            ControllerKind::Apic => {
                self.lapic.eoi();
            }
            ControllerKind::LegacyPic => {
                pic::notify_end_of_interrupt(vector);
            }
        }
    }
}

impl Default for InterruptController {
    fn default() -> Self {
        Self::new()
    }
}

pub static CONTROLLER: SpinMutex<InterruptController> =
    SpinMutex::new(InterruptController::new());

pub fn init() -> ControllerKind {
    CONTROLLER.lock().init()
}

pub fn notify_end_of_interrupt(vector: u8) {
    CONTROLLER.lock().notify_end_of_interrupt(vector);
}
