use crate::arch::x86_64::acpi::InterruptTopology;
use crate::arch::x86_64::apic::{IoApic, LocalApic};
use crate::arch::x86_64::pic;
use crate::sync::IrqSpinMutex;

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
    /// IOAPIC redirection table index for the timer (IRQ 0) after APIC init.
    /// Used to mask the entry once the Local APIC Timer takes over tick delivery.
    pub timer_irq_index: u8,
}

impl InterruptController {
    pub const fn new() -> Self {
        Self {
            lapic: LocalApic::new(),
            ioapic: IoApic::new(),
            secondary_ioapic: None,
            mode: ControllerKind::LegacyPic,
            timer_irq_index: 0,
        }
    }

    pub fn init(&mut self, topology: Option<&InterruptTopology>) -> ControllerKind {
        // 1. Remap legacy PIC offsets first so no vectors collide with CPU exceptions (0..31)
        pic::init();
        #[cfg(not(feature = "apic"))]
        let _ = topology;

        // 2. Attempt APIC initialization only when explicitly enabled and
        // from ACPI-discovered topology.
        #[cfg(feature = "apic")]
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

                    // COM1 receive (ISA IRQ 4), on whichever IOAPIC has it.
                    #[cfg(feature = "serial")]
                    {
                        let (serial_gsi, serial_flags) = topology.gsi_for_irq(4);
                        let lapic_id = self.lapic.id() as u8;
                        let vector = crate::drivers::serial::RECEIVE_VECTOR;
                        match topology.ioapic_for_gsi(serial_gsi) {
                            Some(ioapic) if ioapic.address == timer_ioapic.address => {
                                let index = (serial_gsi - ioapic.gsi_base) as u8;
                                self.ioapic.set_redirection_with_flags(
                                    index,
                                    vector,
                                    lapic_id,
                                    serial_flags,
                                    false,
                                );
                            }
                            Some(ioapic) => {
                                if let Some(secondary) = self
                                    .secondary_ioapic
                                    .as_mut()
                                    .filter(|other| other.base_address() == ioapic.address)
                                {
                                    let index = (serial_gsi - ioapic.gsi_base) as u8;
                                    secondary.set_redirection_with_flags(
                                        index,
                                        vector,
                                        lapic_id,
                                        serial_flags,
                                        false,
                                    );
                                } else {
                                    crate::serial_println!(
                                        "[APIC] COM1 IRQ on another IOAPIC; serial input disabled"
                                    );
                                }
                            }
                            None => {}
                        }
                    }

                    // Remember the IOAPIC table index for timer IRQ so that
                    // apic_timer::init() can mask it once the LAPIC timer is live.
                    self.timer_irq_index = timer_gsi.saturating_sub(timer_ioapic.gsi_base) as u8;

                    self.mode = ControllerKind::Apic;
                    return ControllerKind::Apic;
                }
            }
        }

        // 4. Fallback Path: keep 8259 PIC active with Timer & Keyboard
        // unmasked. APIC mode leaves the legacy PIC fully masked.
        pic::unmask_legacy_irqs();
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

    /// Mask the IOAPIC timer redirection entry so the PIT no longer delivers
    /// Vector 0x20 interrupts once the Local APIC Timer has taken over.
    #[cfg(feature = "apic-timer")]
    pub fn mask_ioapic_timer(&mut self) {
        self.ioapic.mask_timer_irq(self.timer_irq_index);
    }
}

impl Default for InterruptController {
    fn default() -> Self {
        Self::new()
    }
}

/// Every IRQ handler takes this lock to send EOI, so holders must mask
/// interrupts or the next tick would spin on it forever.
pub static CONTROLLER: IrqSpinMutex<InterruptController> =
    IrqSpinMutex::new(InterruptController::new());

pub fn init(topology: Option<&InterruptTopology>) -> ControllerKind {
    CONTROLLER.lock().init(topology)
}

pub fn notify_end_of_interrupt(vector: u8) {
    CONTROLLER.lock().notify_end_of_interrupt(vector);
}

/// The Local APIC ID MSI messages should target, or `None` in legacy PIC
/// mode, where nothing would deliver them (and nothing would take the EOI).
pub fn msi_destination() -> Option<u8> {
    let controller = CONTROLLER.lock();
    (controller.mode == ControllerKind::Apic).then(|| controller.lapic.id() as u8)
}
