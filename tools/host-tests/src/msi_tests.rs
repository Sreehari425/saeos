//! MSI programming against one fake function whose writes are logged, for
//! all four capability layouts. The layouts are where a wrong offset sends
//! the vector into the address register and the interrupt never comes.

use crate::pci::config::{Address, CAPABILITY_MSI, COMMAND, COMMAND_INTX_DISABLE, ConfigSpace};
use crate::pci::device::read_device;
use crate::pci::msi::{self, MsiCapability};
use std::cell::RefCell;

const AT: Address = Address::new(0, 4, 0);
const CAP: u8 = 0x50;
const ENABLE: u32 = 1 << 16;

struct Fake {
    config: RefCell<[u32; 64]>,
    writes: RefCell<Vec<(u8, u32)>>,
}

impl ConfigSpace for Fake {
    fn read32(&self, address: Address, offset: u8) -> u32 {
        assert_eq!(address, AT);
        assert_eq!(offset % 4, 0);
        self.config.borrow()[usize::from(offset / 4)]
    }

    fn write32(&self, address: Address, offset: u8, value: u32) {
        assert_eq!(address, AT);
        assert_eq!(offset % 4, 0);
        // The capability header (id, next) is read-only.
        let value = if offset == CAP {
            (value & 0xffff_0000) | (self.config.borrow()[usize::from(CAP / 4)] & 0xffff)
        } else {
            value
        };
        self.config.borrow_mut()[usize::from(offset / 4)] = value;
        self.writes.borrow_mut().push((offset, value));
    }
}

impl Fake {
    /// A device (`edu`-like) with only an MSI capability at 0x50, control
    /// bits as given, the mask register all set, and stale garbage in the
    /// message registers.
    fn new(wide: bool, mask: bool) -> Self {
        let mut config = [0u32; 64];
        config[0] = 0x1234 | (0x11e8 << 16);
        config[1] = (1 << 4) << 16; // capabilities list
        config[2] = 0x00ff_0000;
        config[13] = u32::from(CAP);
        let control = (u32::from(wide) << 7) | (u32::from(mask) << 8) | (0b010 << 1) | (0b011 << 4);
        config[usize::from(CAP / 4)] = u32::from(CAPABILITY_MSI) | (control << 16);
        for slot in 1..6 {
            config[usize::from(CAP / 4) + slot] = 0xdead_beef;
        }
        Self {
            config: RefCell::new(config),
            writes: RefCell::new(Vec::new()),
        }
    }

    fn dword(&self, offset: u8) -> u32 {
        self.config.borrow()[usize::from(offset / 4)]
    }

    fn capability(&self) -> MsiCapability {
        let device = read_device(self, AT).unwrap();
        let offset = device.has_capability(CAPABILITY_MSI).unwrap();
        MsiCapability::read(self, AT, offset)
    }
}

#[test]
fn layouts() {
    for (wide, mask, data, mask_at) in [
        (false, false, 0x58, None),
        (true, false, 0x5c, None),
        (false, true, 0x58, Some(0x5c)),
        (true, true, 0x5c, Some(0x60)),
    ] {
        let fake = Fake::new(wide, mask);
        let capability = fake.capability();
        assert_eq!(capability.wide, wide);
        assert_eq!(capability.per_vector_mask, mask);
        assert_eq!(capability.vectors, 4);
        assert_eq!(capability.address_low(), 0x54);
        assert_eq!(capability.address_high(), wide.then_some(0x58));
        assert_eq!(capability.data(), data);
        assert_eq!(capability.mask(), mask_at);
    }
}

#[test]
fn program_writes_the_message_then_enables() {
    for wide in [false, true] {
        for mask in [false, true] {
            let fake = Fake::new(wide, mask);
            let capability = fake.capability();
            fake.writes.borrow_mut().clear();
            msi::program(&fake, AT, &capability, 3, 0x41);

            assert_eq!(fake.dword(0x54), 0xfee0_3000, "address, wide={wide}");
            if wide {
                assert_eq!(fake.dword(0x58), 0);
                // Data is the low half; the upper half keeps its bits.
                assert_eq!(fake.dword(0x5c) & 0xffff, 0x41);
            } else {
                assert_eq!(fake.dword(0x58) & 0xffff, 0x41);
            }
            if let Some(mask_at) = capability.mask() {
                assert_eq!(fake.dword(mask_at) & 1, 0, "vector 0 unmasked");
            }
            let control = fake.dword(CAP) >> 16;
            assert_eq!(control & 1, 1, "enabled");
            assert_eq!((control >> 4) & 0x7, 0, "one vector");
            assert_eq!((control >> 1) & 0x7, 0b010, "capable field untouched");
            assert_eq!(fake.dword(CAP) & 0xffff, u32::from(CAPABILITY_MSI));
            assert_ne!(fake.read16(AT, COMMAND) & COMMAND_INTX_DISABLE, 0);
            assert!(capability.is_enabled(&fake, AT));

            let writes = fake.writes.borrow();
            // Off before anything changes, on as the very last write.
            assert_eq!(writes.first().unwrap().0, CAP);
            assert_eq!(writes.first().unwrap().1 & ENABLE, 0);
            assert_eq!(*writes.last().unwrap(), (CAP, fake.dword(CAP)));
            let enabled_at = writes
                .iter()
                .position(|&(offset, value)| offset == CAP && value & ENABLE != 0)
                .unwrap();
            assert_eq!(enabled_at, writes.len() - 1);
        }
    }
}

#[test]
fn disable_undoes_it() {
    let fake = Fake::new(true, true);
    let capability = fake.capability();
    msi::program(&fake, AT, &capability, 0, 0x40);
    msi::disable(&fake, AT, &capability);
    assert!(!capability.is_enabled(&fake, AT));
    assert_eq!(fake.read16(AT, COMMAND) & COMMAND_INTX_DISABLE, 0);
}

#[test]
fn no_capability_no_msi() {
    let fake = Fake::new(false, false);
    // Status bit 4 clear: the capability list does not exist.
    fake.config.borrow_mut()[1] = 0;
    let device = read_device(&fake, AT).unwrap();
    assert_eq!(device.has_capability(CAPABILITY_MSI), None);
}

#[test]
fn message_address() {
    assert_eq!(msi::message_address(0), 0xfee0_0000);
    assert_eq!(msi::message_address(0xff), 0xfeef_f000);
}
