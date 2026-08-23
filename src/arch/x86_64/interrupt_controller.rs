use crate::arch::x86_64::acpi::InterruptTopology;
use crate::arch::x86_64::apic::{IoApic, LocalApic};
use crate::arch::x86_64::pic;
use crate::sync::SpinMutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerKind {
    Apic,
    LegacyPic,
}

pub struct InterruptController {
    pub lapic: LocalApic,
    pub ioapic: IoApic,
    pub secondary_ioapic: Option<IoApic>,
    pub mode: ControllerKind,
}

impl InterruptController {
    pub const fn new() -> Self {
        Self {
            lapic: LocalApic::new(),
            ioapic: IoApic::new(),
            secondary_ioapic: None,
            mode: ControllerKind::LegacyPic,
        }
    }

    pub fn init(&mut self, topology: Option<&InterruptTopology>) -> ControllerKind {
        // 1. Remap legacy PIC offsets first so no vectors collide with CPU exceptions (0..31)
        pic::init();

        // 2. Attempt APIC initialization only from ACPI-discovered topology.
        if let Some(topology) = topology {
            let (timer_gsi, timer_flags) = topology.gsi_for_irq(0);
            let (keyboard_gsi, keyboard_flags) = topology.gsi_for_irq(1);
            let timer_ioapic = topology.ioapic_for_gsi(timer_gsi);
            let keyboard_ioapic = topology.ioapic_for_gsi(keyboard_gsi);
            if let (Some(timer_ioapic), Some(keyboard_ioapic)) = (timer_ioapic, keyboard_ioapic) {
                self.lapic.set_base_address(topology.lapic_address);
                self.ioapic.set_base_address(timer_ioapic.address);

                let lapic_ready = self.lapic.init().is_ok();
                let same_ioapic = timer_ioapic.address == keyboard_ioapic.address;
                let timer_ready = if same_ioapic {
                    lapic_ready
                        && self
                            .ioapic
                            .init(
                                self.lapic.id() as u8,
                                timer_ioapic.gsi_base,
                                timer_gsi,
                                timer_flags,
                                keyboard_gsi,
                                keyboard_flags,
                            )
                            .is_ok()
                } else {
                    lapic_ready
                        && self
                            .ioapic
                            .init_single(
                                self.lapic.id() as u8,
                                timer_ioapic.gsi_base,
                                timer_gsi,
                                0x20,
                                timer_flags,
                            )
                            .is_ok()
                };

                let keyboard_ready = if same_ioapic {
                    timer_ready
                } else {
                    let mut secondary = IoApic::new();
                    secondary.set_base_address(keyboard_ioapic.address);
                    let ready = lapic_ready
                        && secondary
                            .init_single(
                                self.lapic.id() as u8,
                                keyboard_ioapic.gsi_base,
                                keyboard_gsi,
                                0x21,
                                keyboard_flags,
                            )
                            .is_ok();
                    if ready {
                        self.secondary_ioapic = Some(secondary);
                    }
                    ready
                };

                if timer_ready && keyboard_ready {
                    // Completely mask 8259 PIC to disable legacy IRQ routing
                    pic::mask_all();

                    self.mode = ControllerKind::Apic;
                    return ControllerKind::Apic;
                }
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

pub static CONTROLLER: SpinMutex<InterruptController> = SpinMutex::new(InterruptController::new());

pub fn init(topology: Option<&InterruptTopology>) -> ControllerKind {
    CONTROLLER.lock().init(topology)
}

pub fn notify_end_of_interrupt(vector: u8) {
    CONTROLLER.lock().notify_end_of_interrupt(vector);
}
