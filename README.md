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
- Higher-half kernel (top 2 GiB) on both boot paths; the lower half is left
  empty for user address spaces
- Buddy physical frame allocator with per-frame reference counts, and paging
- A kernel heap (small-object classes plus a page-backed heap)
- Guarded kernel stacks with stack-overflow detection
- A preemptive kernel task scheduler (EEVDF)
- A VFS with ramfs (`/`), devfs (`/dev`), procfs (`/proc`), and
  per-process file descriptors
- A cpio initramfs loaded by the bootloader, and a Linux-style kernel
  command line (`/proc/cmdline`)
- Userspace in ring 3: boots to `/sbin/init` and an interactive `/bin/sh`
  with `ls`, `cat`, `echo`, `mkdir`, `rm`, `mv`, `stat`; Linux-numbered
  syscalls (`fork` with copy-on-write, `execve`, `wait4`, `mmap`, ...),
  console line editing, and libsaeos, a C library in Rust
- PCI enumeration (buses behind bridges, BARs, capabilities) with a
  driver-matching layer; `lspci` and `/proc/pci`
- DMA buffers and MSI interrupts for drivers
- Sound: Intel HD Audio and AC'97 drivers behind `/dev/dsp` (the OSS
  interface), with sample-rate conversion, and `play` for WAV files
- `/dev/fb0` for programs, and `badapple`, which plays Bad Apple!! in
  black and white, in step with its sound
- Clocks and sleeping for programs (`clock_gettime`, `nanosleep`, `sleep`)
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
- [x] Tasks
- [x] Scheduler
- [x] Context switching
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

Add a sound card you can hear with `SOUND`, naming a QEMU audio backend
(`pipewire`, `pa`, `alsa`, `sdl`, ...):

```sh
make run-debug SOUND=pipewire                 # AC'97
make run-debug SOUND=pipewire SOUND_CARD=hda  # Intel HD Audio
```

QEMU uses KVM when the host has `/dev/kvm`; `KVM=false` turns it off
(every run and test target takes it), and `KVM=true` insists on it.

To play Bad Apple!!, convert the video once (needs `ffmpeg`;
`BAD_APPLE_SRC` is a directory holding `video.mp4`, by default
`../bad-apple-terminal/data`). The result goes into the git-ignored
`initramfs/badapple/`; then run `badapple` in the shell:

```sh
make badapple-data BAD_APPLE_SRC=path/to/data
make run-debug SOUND=pipewire
```

### A bootable ISO

`make iso` writes `target/saeos.iso`, which boots under BIOS (through
GRUB) and UEFI, as a CD or written to a USB stick; `nix build .#iso` builds
the same from the flake. The command line is built in
(`make iso CMDLINE="loglevel=debug"`). 
```sh
make iso
sudo dd if=target/saeos.iso of=/dev/sdX bs=4M conv=fsync   # check sdX first: it is overwritten
```

On real hardware, turn Secure Boot off (the kernel is not signed). Under
UEFI from a CD, the initramfs must stay under about 30 MiB (El Torito's
limit); from a USB stick any size boots. `make iso-test` boots an ISO in
QEMU all four ways.

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
make test          # All tests below plus the fault tests, on BIOS and UEFI
make user-test     # Ring-3 tests: a test program runs as init (PID 1)
make sound-test    # Play tones on AC'97 and HD Audio cards and check QEMU's recording
make fb-test       # Draw on /dev/fb0 and check QEMU screendumps, pixel for pixel
make badapple-test # Play a short test clip and check its last frame and its sound
make host-test     # Host tests (tools/host-tests): kernel and userspace code built for the host
make miri          # The same host tests under Miri (undefined-behaviour checks)
make mm-stress     # Boot with MM self-tests and stress runs (also mm-stress-uefi)
make mm-guard-test # Overflow a kernel stack on purpose and check it is reported
make asm           # Emit BIOS and UEFI assembly
make userspace     # Build user programs for userspace/x86_64-saeos.json
make initramfs     # Build target/initramfs.cpio from initramfs.toml
make clean         # Remove Cargo build artifacts
```

The kernel is written in Rust and owns the platform, memory, and driver layers
it runs on.

## Userspace

A normal boot starts `/sbin/init`, which runs `/bin/sh` on the console
(type `help` there). Boot with `CMDLINE="init=/bin/sh"` to skip init, or
`CMDLINE=saeos.kshell` for the in-kernel debug shell. Programs live in
`userspace/` and build with `make userspace`.

## Initramfs and command line

Files in `initramfs/` appear at `/` after boot. `initramfs.toml` lists what
goes into the image, and `make initramfs` builds it. Pass kernel parameters
with `CMDLINE`, for example `make run-debug CMDLINE="loglevel=debug"`; the
kernel shows them in `/proc/cmdline`. See `kernel/src/fs/mod.rs` and
`kernel/src/cmdline.rs` for the details.

## Debugging

Panics and CPU exceptions print a backtrace. Resolve it to functions and
source lines with:

```sh
make symbolize LOG=crash.log   # or: scripts/symbolize.sh crash.log
```

Attach GDB by starting QEMU halted in one terminal and the debugger in
another:

```sh
make debug     # BIOS kernel, waits for GDB on :1234
make gdb       # source-level: breaks at kernel_main_bios
```

`make debug-uefi` / `make gdb-uefi` work the same way at the assembly level
(the UEFI build's debug info is PDB, which GDB cannot read); `ksym [addr]`
in that session resolves addresses to source. The shell's `dmesg` shows the
kernel log, and `loglevel debug` enables debug tracing at runtime.

## Project layout

```text
kernel/             The SaeOS kernel
kernel/src/arch/    x86_64 boot and CPU-specific code
kernel/src/boot/    BIOS/UEFI boot information
kernel/src/mm/      Physical memory, paging, and heap code
kernel/src/drivers/ Console and hardware drivers
kernel/src/time/    Timer implementations
tools/ratconf/      Configuration tool
tools/mkinitramfs/  Initramfs (cpio) builder
userspace/          User programs and libsaeos, their C library in Rust (/bin)
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
