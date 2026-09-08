# SaeOS

> A small bare-metal operating system written in Rust.

SaeOS is a work in progress x86_64 operating-system project. The goal is to
learn by building the whole stack from booting and memory management to tasks,
userspace, filesystems, and whatever other madness comes next.

This is early-stage, experimental software. Expect rough edges, incomplete
subsystems, breaking changes, and the occasional kernel panic.

## Current status

The project can boot as a 64-bit kernel through both legacy BIOS and UEFI and
run inside QEMU. The current codebase includes:

- Rust `no_std` kernel code
- BIOS boot support and UEFI boot support
- GDT, TSS, IDT, and interrupt handling
- Legacy 8259 PIC and APIC/IOAPIC support
- Physical frame allocation and paging
- A kernel heap
- VGA text and UEFI GOP framebuffer consoles
- Serial output, keyboard input, and basic shell commands
- PIT, RTC, TSC, and local APIC timer support
- Compile-time configuration through `ratconf`

## Roadmap

The roadmap is intentionally ambitious. The order may change as the kernel
grows.

- [x] Boot
- [x] GDT / IDT
- [x] Interrupt controllers
- [x] Physical memory management
- [x] Paging
- [x] Heap
- [x] Basic drivers
- [x] Timer
- [ ] Tasks
- [ ] Scheduler
- [ ] Context switching
- [ ] User mode
- [ ] Syscalls
- [ ] Processes
- [ ] Filesystem
- [ ] Networking
- [ ] Better hardware support
- [ ] Rewrite the build system for fun
- [ ] ...whatever madness comes next

## Building

### Recommended: Nix

The flake provides the pinned Rust toolchain and the required build tools.

Build and run the BIOS image:

```sh
nix build .#bios
nix run .#bios
```

Build and run the UEFI image:

```sh
nix build .#uefi
nix run .#uefi
```

To enter the development shell:

```sh
nix develop
```

### Using the local toolchain

You need Rust nightly as specified in [`rust-toolchain.toml`](rust-toolchain.toml),
NASM, GNU binutils, QEMU, and GNU Make. The Rust targets used by the kernel are
`x86_64-unknown-none` and `x86_64-unknown-uefi`.

```sh
make bios       # Build the BIOS kernel
make uefi       # Build the UEFI kernel
make run        # Build and boot the BIOS kernel in QEMU
make run-uefi   # Build and boot the UEFI kernel in QEMU
```

For a faster debug-oriented build that uses the scripts in `scripts/`:

```sh
make run-debug
make run-uefi-debug
```

Pass extra QEMU arguments with `QEMU_ARGS`:

```sh
make run-uefi QEMU_ARGS="-m 1G -smp 2"
```

## Configuration

SaeOS uses a small, declarative kernel configuration system.
The generated `.config` file controls
which kernel features are enabled for a particular build.

```sh
make menuconfig    # Configure interactively
make defconfig     # Use the normal default configuration
make tinyconfig    # Use a minimal configuration
make olddefconfig  # Refresh an existing configuration
make savedefconfig # Save the current minimal configuration
```

The checked-in configuration templates live in [`configs/`](configs/).

## Checks and development commands

```sh
make check         # Check BIOS and UEFI kernel builds
make fmt-check     # Check formatting
make clippy        # Run Clippy with warnings denied
make asm           # Emit BIOS and UEFI assembly
make clean         # Remove Cargo build artifacts
```

The kernel is written in Rust and owns the platform, memory, and driver layers
it runs on.

## Project layout

```text
kernel/             The SaeOS kernel
kernel/src/arch/    x86_64 boot and CPU-specific code
kernel/src/boot/    BIOS/UEFI boot information
kernel/src/mm/      Physical memory, paging, and heap code
kernel/src/drivers/ Console and hardware drivers
kernel/src/time/    Timer implementations
tools/ratconf/      Configuration tool
scripts/            QEMU runner scripts
configs/            Example configuration files
```

## Contributing

Issues, ideas, experiments, documentation improvements, and code contributions
are welcome. Since the project is still evolving, it is a good idea to open an
issue or discussion before starting a large subsystem.

Some useful areas include:

- fixing boot or QEMU issues
- improving hardware and bootloader support
- writing tests and memory-manager self-tests
- documenting kernel internals
- working on any item in the roadmap

Please keep changes focused and run `make check`, `make fmt-check`, and
`make clippy` before submitting a patch when possible.

## License

SaeOS is free software released under the [GNU Affero General Public License
version 3](LICENSE).
