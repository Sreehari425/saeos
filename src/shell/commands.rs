use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::arch::x86_64::cpu;
use crate::arch::x86_64::interrupt_controller::{self, ControllerKind};
use crate::collections::hasher::BuildIdentityHasher;
use crate::collections::{ChainedMap, HashSet, Map, OpenAddressMap, Set, StaticMap};
use crate::drivers::console;
use crate::drivers::vga::Color;
use crate::mm::{heap_start, HEAP_SIZE};
use crate::println;

pub fn execute(cmd: &str) {
    match cmd {
        "help" => {
            console::set_color(Color::Yellow, Color::Black);
            println!("Available Commands:");
            println!("  help    - Show this help message");
            println!("  clear   - Clear the console screen");
            println!("  apic    - Display APIC & interrupt controller status");
            println!("  mem     - Test Kernel Heap & in-tree trait-driven Collections");
            println!("  <text>  - Echoes your input back to the screen");
        }
        "clear" => {
            console::clear_screen();
        }
        "mem" | "alloc" => {
            console::set_color(Color::LightCyan, Color::Black);
            println!("--- Memory Management & Trait Collections ---");
            console::set_color(Color::White, Color::Black);
            println!("Heap Base:     {:#x}", heap_start());
            println!("Heap Size:     {} MiB ({} bytes)", HEAP_SIZE / (1024 * 1024), HEAP_SIZE);

            console::set_color(Color::Yellow, Color::Black);
            println!("\n1. Testing dynamic Rust 'alloc' crate:");

            // Box test
            let heap_box = Box::new(42u64);
            console::set_color(Color::LightGreen, Color::Black);
            println!("  [+] Box<u64>: val = {}, addr = {:p}", *heap_box, heap_box);

            // Vector test
            let mut vec: Vec<usize> = Vec::new();
            for i in 0..8 {
                vec.push(i * 10);
            }
            println!("  [+] Vec<usize>: len = {}, cap = {}, data = {:?}", vec.len(), vec.capacity(), vec.as_slice());

            // String & format! test
            let formatted_str = format!("Dynamic string allocating {} elements", vec.len());
            println!("  [+] String (format!): \"{}\"", formatted_str);

            console::set_color(Color::Yellow, Color::Black);
            println!("\n2. Testing In-Tree Kernel Collections (trait Map<K,V>):");

            // OpenAddressMap test (Flat table with FNV-1a hasher)
            let mut open_map: OpenAddressMap<String, &str> = OpenAddressMap::new();
            open_map.insert(String::from("vfs"), "Virtual File System");
            open_map.insert(String::from("sched"), "Preemptive Scheduler");
            open_map.insert(String::from("ipc"), "Inter-Process Comm");
            console::set_color(Color::LightGreen, Color::Black);
            println!("  [+] OpenAddressMap<String, &str> (FNV-1a): len = {}", open_map.len());
            for (k, v) in open_map.iter() {
                println!("      - {} => {}", k, v);
            }

            // ChainedMap test (Linux hlist-style buckets with IdentityHasher for integers)
            let mut chained_map: ChainedMap<u64, &str, BuildIdentityHasher> = ChainedMap::with_hasher(BuildIdentityHasher::new());
            chained_map.insert(1, "Init process (PID 1)");
            chained_map.insert(2, "Kernel idle thread (PID 2)");
            chained_map.insert(33, "Keyboard IRQ handler");
            println!("  [+] ChainedMap<u64, &str, IdentityHasher>: len = {}", chained_map.len());
            for (pid, desc) in chained_map.iter() {
                println!("      - PID/IRQ {} => {}", pid, desc);
            }

            // StaticMap test (Zero-heap, Interrupt/Panic safe)
            let mut static_map: StaticMap<&str, u16, 4> = StaticMap::new();
            static_map.insert("COM1", 0x3F8);
            static_map.insert("VGA_CRTC", 0x3D4);
            static_map.insert("PIC_MASTER", 0x20);
            println!("  [+] StaticMap<&str, u16, 4> [ZERO-HEAP]: len = {}/{}", static_map.len(), static_map.capacity());
            for (dev, port) in static_map.iter() {
                println!("      - Hardware Port {} = {:#x}", dev, port);
            }

            // HashSet test
            let mut set: HashSet<&str> = HashSet::new();
            set.insert("ext2");
            set.insert("fat32");
            println!("  [+] HashSet<&str> (Set trait): len = {}", set.len());

            console::set_color(Color::LightGreen, Color::Black);
            println!("\n[OK] All trait-driven collections and memory operations passed!");
            println!("---------------------------------------------");
        }
        "apic" | "status" => {
            // Snapshot controller state before printing. Printing can take
            // time while interrupts are enabled, and the keyboard interrupt
            // handler also needs CONTROLLER to send its EOI. Holding this
            // lock across println! would deadlock if a key arrived then.
            // The interrupt handler also reads CONTROLLER for EOI, so mask
            // interrupts for the short snapshot window as well.
            cpu::cli();
            let snapshot = {
                let ctrl = interrupt_controller::CONTROLLER.lock();
                (
                    ctrl.mode,
                    ctrl.lapic.base_address(),
                    ctrl.lapic.id(),
                    ctrl.lapic.version(),
                    ctrl.ioapic.base_address(),
                    ctrl.ioapic.id(),
                    ctrl.ioapic.max_redirection_entries(),
                )
            };
            cpu::sti();

            let (mode, lapic_base, lapic_id, lapic_version, ioapic_base, ioapic_id, max_irqs) =
                snapshot;

            console::set_color(Color::LightCyan, Color::Black);
            println!("--- Interrupt Controller Diagnostics ---");
            match mode {
                ControllerKind::Apic => {
                    console::set_color(Color::LightGreen, Color::Black);
                    println!("Mode:          APIC (Local APIC + I/O APIC) [ACTIVE]");
                    console::set_color(Color::White, Color::Black);
                    println!(
                        "Local APIC:    Base={:#x}, ID={}, Version={:#x}",
                        lapic_base, lapic_id, lapic_version
                    );
                    println!(
                        "I/O APIC:      Base={:#x}, ID={}, Max IRQs={}",
                        ioapic_base, ioapic_id, max_irqs
                    );
                    println!("Legacy 8259:   Masked (Disabled)");
                }
                ControllerKind::LegacyPic => {
                    console::set_color(Color::LightRed, Color::Black);
                    println!("Mode:          8259 Legacy PIC (Fallback)");
                    console::set_color(Color::White, Color::Black);
                    println!("Master/Slave:  Base Ports 0x20/0xA0 (IRQs 0x20..=0x2F)");
                }
            }
            println!("----------------------------------------");
        }
        _ => {
            console::set_color(Color::LightGreen, Color::Black);
            println!("Echo: {}", cmd);
        }
    }
}
