use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::arch::x86_64::interrupt_controller::{self, ControllerKind};
use crate::drivers::vga::{self, Color};
use crate::mm::{HEAP_SIZE, HEAP_START};
use crate::println;

pub fn execute(cmd: &str) {
    match cmd {
        "help" => {
            vga::set_color(Color::Yellow, Color::Black);
            println!("Available Commands:");
            println!("  help    - Show this help message");
            println!("  clear   - Clear the VGA text screen");
            println!("  apic    - Display APIC & interrupt controller status");
            println!("  mem     - Test and display Kernel Heap & dynamic memory allocation");
            println!("  <text>  - Echoes your input back to the screen");
        }
        "clear" => {
            vga::clear_screen();
        }
        "mem" | "alloc" => {
            vga::set_color(Color::LightCyan, Color::Black);
            println!("--- Memory Management & Heap Status ---");
            vga::set_color(Color::White, Color::Black);
            println!("Heap Base:     {:#x}", HEAP_START);
            println!("Heap Size:     {} MiB ({} bytes)", HEAP_SIZE / (1024 * 1024), HEAP_SIZE);

            vga::set_color(Color::Yellow, Color::Black);
            println!("\nTesting dynamic Rust 'alloc' crate:");

            // 1. Box test
            let heap_box = Box::new(42u64);
            vga::set_color(Color::LightGreen, Color::Black);
            println!("  [+] Box<u64>: val = {}, addr = {:p}", *heap_box, heap_box);

            // 2. Vector test
            let mut vec: Vec<usize> = Vec::new();
            for i in 0..8 {
                vec.push(i * 10);
            }
            println!("  [+] Vec<usize>: len = {}, cap = {}, data = {:?}", vec.len(), vec.capacity(), vec.as_slice());

            // 3. String & format! test
            let formatted_str = format!("Dynamic string allocating {} elements", vec.len());
            println!("  [+] String (format!): \"{}\"", formatted_str);

            // 4. BTreeMap test
            let mut map = BTreeMap::new();
            map.insert(String::from("kernel"), "SaeOS");
            map.insert(String::from("arch"), "x86_64");
            map.insert(String::from("allocator"), "LinkedList");
            println!("  [+] BTreeMap<String, &str>: entries = {}", map.len());
            for (k, v) in &map {
                println!("      - {} => {}", k, v);
            }

            // 5. External crate HashMap (hashbrown) test
            let mut hash_map = hashbrown::HashMap::new();
            hash_map.insert("crate", "hashbrown");
            hash_map.insert("status", "working in #![no_std]");
            println!("  [+] HashMap (hashbrown crate): entries = {}", hash_map.len());
            for (k, v) in &hash_map {
                println!("      - {} => {}", k, v);
            }

            vga::set_color(Color::LightGreen, Color::Black);
            println!("\n[OK] All dynamic allocations, collections, and drops OK!");
            println!("---------------------------------------");
        }
        "apic" | "status" => {
            let ctrl = interrupt_controller::CONTROLLER.lock();
            vga::set_color(Color::LightCyan, Color::Black);
            println!("--- Interrupt Controller Diagnostics ---");
            match ctrl.mode {
                ControllerKind::Apic => {
                    vga::set_color(Color::LightGreen, Color::Black);
                    println!("Mode:          APIC (Local APIC + I/O APIC) [ACTIVE]");
                    vga::set_color(Color::White, Color::Black);
                    println!(
                        "Local APIC:    Base={:#x}, ID={}, Version={:#x}",
                        ctrl.lapic.base_address(),
                        ctrl.lapic.id(),
                        ctrl.lapic.version()
                    );
                    println!(
                        "I/O APIC:      Base={:#x}, ID={}, Max IRQs={}",
                        ctrl.ioapic.base_address(),
                        ctrl.ioapic.id(),
                        ctrl.ioapic.max_redirection_entries()
                    );
                    println!("Legacy 8259:   Masked (Disabled)");
                }
                ControllerKind::LegacyPic => {
                    vga::set_color(Color::LightRed, Color::Black);
                    println!("Mode:          8259 Legacy PIC (Fallback)");
                    vga::set_color(Color::White, Color::Black);
                    println!("Master/Slave:  Base Ports 0x20/0xA0 (IRQs 0x20..=0x2F)");
                }
            }
            println!("----------------------------------------");
        }
        _ => {
            vga::set_color(Color::LightGreen, Color::Black);
            println!("Echo: {}", cmd);
        }
    }
}
