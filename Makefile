RATCONF := cargo run --quiet -p ratconf --
KERNEL := saeos
BIOS_TARGET := x86_64-unknown-none
UEFI_TARGET := x86_64-unknown-uefi
QEMU_ARGS ?=
# A sound card to hear: `make run SOUND=pipewire` (or pa, alsa, sdl, ...)
# adds a card played through that QEMU audio backend: AC'97, or HD Audio
# with SOUND_CARD=hda.
SOUND ?=
SOUND_CARD ?= ac97
# KVM: guests run on the host CPU instead of being emulated (TCG), many
# times faster. On wherever the host has /dev/kvm; `KVM=false` emulates.
KVM_AVAILABLE := $(if $(wildcard /dev/kvm),true,false)
KVM ?= $(KVM_AVAILABLE)
ifeq ($(KVM),true)
ifneq ($(KVM_AVAILABLE),true)
$(error KVM=true, but this host has no /dev/kvm (use KVM=false))
endif
ACCEL_ARGS := -accel kvm
else ifeq ($(KVM),false)
ACCEL_ARGS :=
else
$(error KVM must be true or false, not '$(KVM)')
endif
# Memory for `make run*`: QEMU's default 128 MiB is too little for a big
# initramfs (UEFI needs it in one piece, and the kernel unpacks a copy).
# QEMU_ARGS come after it, so `QEMU_ARGS="-m 4G"` still wins.
RUN_MEM ?= 1G
COMMA := ,
AC97_DEVICES = -device AC97$(COMMA)audiodev=snd
HDA_DEVICES = -device intel-hda -device hda-output$(COMMA)audiodev=snd
SOUND_DEVICES = $(if $(filter hda,$(SOUND_CARD)),$(HDA_DEVICES),$(AC97_DEVICES))
SOUND_ARGS = $(if $(SOUND),-audiodev $(SOUND)$(COMMA)id=snd $(SOUND_DEVICES))
FEATURES = $(shell if [ -f .config ]; then cargo run --quiet -p ratconf -- features 2>/dev/null; fi)
FEATURE_ARGS = $(if $(FEATURES),--features $(FEATURES),)

# The initramfs (built from initramfs.toml) and kernel command line given to
# the run and debug targets: make run-debug CMDLINE="loglevel=debug".
MKINITRAMFS := cargo run --quiet -p mkinitramfs --
INITRAMFS_IMAGE := target/initramfs.cpio
INITRAMFS_TEST_IMAGE := target/initramfs-test.cpio
CMDLINE ?=
BOOT_ENV = INITRAMFS=$(INITRAMFS_IMAGE) CMDLINE='$(CMDLINE)'

# Fixed feature sets for the automated QEMU tests, independent of .config.
TEST_FEATURES := serial,apic,apic-timer,pit,tsc,keyboard,ac97,hda,bochs-vbe,procfs,kernel-shell
# The fewest features a kernel builds with (configs/tinyconfig.toml).
TINY_FEATURES := serial,vga-text
BIOS_TEST_FEATURES := $(TEST_FEATURES),vga-text
UEFI_TEST_FEATURES := $(TEST_FEATURES),framebuffer
# Enough RAM to exercise frames above 4 GiB.
TEST_MEM ?= 6G
# The tests boot the image with /test fixtures and check this marker.
TEST_CMDLINE := loglevel=info saeos.test=boot
QEMU_TEST := INITRAMFS=$(INITRAMFS_TEST_IMAGE) CMDLINE='$(TEST_CMDLINE)' ./scripts/run-qemu-test.sh
# Every QEMU test runs one of these (options after the kernel go to QEMU).
BIOS_TEST_IMAGE := ./scripts/runner-bios-x86_64.sh target/$(BIOS_TARGET)/debug/$(KERNEL) $(ACCEL_ARGS)
UEFI_TEST_IMAGE := ./scripts/runner-uefi-x86_64.sh target/$(UEFI_TARGET)/debug/$(KERNEL).efi $(ACCEL_ARGS)

.PHONY: check-std-lock userspace initramfs initramfs-test all bios uefi asm asm-bios asm-uefi run run-uefi run-debug run-uefi-debug debug debug-uefi gdb gdb-uefi symbolize check menuconfig defconfig tinyconfig olddefconfig savedefconfig
.PHONY: headers_check headers_install clean distclean mrproper nix-gc fmt-check clippy host-test mm-test miri mm-stress mm-stress-uefi mm-guard-test fault-test panic-lock-test boot-test user-test sound-test fb-test badapple-test badapple-data test help

all: bios

# User programs, built for the SaeOS target (userspace/x86_64-saeos.json:
# static, SSE, linked with userspace/user.ld) with core and alloc built from
# source; installed by initramfs.toml. Each must be a static executable
# whose entry point is in its executable segment.
USER_PROGRAMS := hello usertest init sh coreutils
# The executables they produce (checked, and installed by initramfs.toml).
USER_BINARIES := hello usertest init sh ls cat echo mkdir rm mv stat true false lspci sleep play fbinfo fbtest badapple
USER_TARGET := userspace/x86_64-saeos.json
USER_BUILD = cargo build --release -Zjson-target-spec \
	-Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem \
	--target $(USER_TARGET)
userspace:
	$(USER_BUILD) $(addprefix -p ,$(USER_PROGRAMS))
	@for program in $(USER_BINARIES); do \
		./scripts/check-user-elf.sh target/x86_64-saeos/release/$$program || exit 1; \
	done

# The normal image must never carry the test fixtures.
initramfs: userspace
	$(MKINITRAMFS) build initramfs.toml -o $(INITRAMFS_IMAGE)
	@if $(MKINITRAMFS) list $(INITRAMFS_IMAGE) | grep -q ' /test/'; then \
		echo "initramfs: $(INITRAMFS_IMAGE) contains /test" >&2; exit 1; fi

initramfs-test: userspace
	./scripts/make-test-tone.py target/test-tone.wav
	./scripts/make-test-clip.py target
	$(MKINITRAMFS) build initramfs.toml -o $(INITRAMFS_TEST_IMAGE) --test-fixtures

bios: olddefconfig
	nix build path:.#bios

uefi: olddefconfig
	nix build path:.#uefi

asm: asm-bios asm-uefi

asm-bios: olddefconfig
	cargo rustc -p $(KERNEL) \
		--target $(BIOS_TARGET) \
		$(FEATURE_ARGS) \
		-- \
		--emit=asm

asm-uefi: olddefconfig
	cargo rustc -p $(KERNEL) \
		--target $(UEFI_TARGET) \
		$(FEATURE_ARGS) \
		-- \
		--emit=asm

run: olddefconfig
	CMDLINE='$(CMDLINE)' nix run path:.#bios -- -m $(RUN_MEM) $(ACCEL_ARGS) $(QEMU_ARGS) $(SOUND_ARGS)

run-uefi: olddefconfig
	CMDLINE='$(CMDLINE)' nix run path:.#uefi -- -m $(RUN_MEM) $(ACCEL_ARGS) $(QEMU_ARGS) $(SOUND_ARGS)

run-debug: olddefconfig initramfs
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) $(FEATURE_ARGS)
	$(BOOT_ENV) ./scripts/runner-bios-x86_64.sh target/$(BIOS_TARGET)/debug/$(KERNEL) -m $(RUN_MEM) $(ACCEL_ARGS) $(QEMU_ARGS) $(SOUND_ARGS)

run-uefi-debug: olddefconfig initramfs
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) $(FEATURE_ARGS)
	$(BOOT_ENV) ./scripts/runner-uefi-x86_64.sh target/$(UEFI_TARGET)/debug/$(KERNEL).efi -m $(RUN_MEM) $(ACCEL_ARGS) $(QEMU_ARGS) $(SOUND_ARGS)

# Start QEMU halted with a GDB stub on :1234, then attach with `make gdb`
# (or `make gdb-uefi`) from a second terminal.
debug: olddefconfig initramfs
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) $(FEATURE_ARGS)
	$(BOOT_ENV) ./scripts/runner-bios-x86_64.sh target/$(BIOS_TARGET)/debug/$(KERNEL) -s -S -no-shutdown -m $(RUN_MEM) $(ACCEL_ARGS) $(QEMU_ARGS)

debug-uefi: olddefconfig initramfs
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) $(FEATURE_ARGS)
	$(BOOT_ENV) ./scripts/runner-uefi-x86_64.sh target/$(UEFI_TARGET)/debug/$(KERNEL).efi -s -S -no-shutdown -m $(RUN_MEM) $(ACCEL_ARGS) $(QEMU_ARGS)

gdb:
	gdb -x scripts/gdbinit target/$(BIOS_TARGET)/debug/$(KERNEL)

gdb-uefi:
	gdb -x scripts/gdbinit-uefi

# Resolve the backtrace in a saved log: make symbolize LOG=crash.log
symbolize:
	./scripts/symbolize.sh $(LOG)

check: olddefconfig check-std-lock
	cargo check -p $(KERNEL) --target $(BIOS_TARGET) $(FEATURE_ARGS)
	cargo check -p $(KERNEL) --target $(UEFI_TARGET) $(FEATURE_ARGS)
	cargo check -p ratconf
	$(RATCONF) check
	cargo check -p mkinitramfs
	cargo check -Zjson-target-spec -Zbuild-std=core,alloc --target $(USER_TARGET) -p libsaeos $(addprefix -p ,$(USER_PROGRAMS))

# The Nix build vendors the standard library's dependencies from this copy;
# refresh it with the toolchain: cp "$$(rustc --print sysroot)/lib/rustlib/src/rust/library/Cargo.lock" userspace/std-library.Cargo.lock
check-std-lock:
	@cmp -s userspace/std-library.Cargo.lock "$$(rustc --print sysroot)/lib/rustlib/src/rust/library/Cargo.lock" \
		|| { echo "userspace/std-library.Cargo.lock is stale for this toolchain" >&2; exit 1; }

# Kernel and userspace code built for the host and tested there
# (tools/host-tests), plus mkinitramfs. `mm-test` is the old name.
host-test:
	cargo test -p host-tests
	cargo test -p mkinitramfs

mm-test: host-test

# The host tests under Miri, which checks for undefined behaviour the
# normal tests cannot see.
miri:
	cargo miri test -p host-tests

# Host tests plus every QEMU test on both boot paths; each run ends with the
# kernel's own verdict through isa-debug-exit.
test: host-test miri boot-test user-test sound-test fb-test badapple-test mm-guard-test fault-test panic-lock-test

# Each target boots twice: on QEMU's default CPU, and with -cpu max, where
# SMEP/SMAP exist and the user-memory test checks they are enforced.
SMAP_EXPECT := QEMU_TEST_EXPECT="uaccess smap=true smep=true"
# The boot test also checks PCI enumeration: an AC'97 sound card is added,
# and the q35 runs put it behind a PCI-to-PCI bridge (bus 1). QEMU's edu
# teaching device tests DMA buffers and MSI; its full DMA mask makes it use
# the address it is given, untruncated.
EDU := edu,dma_mask=0xffffffffffffffff
PCI_TEST_DEVICES := -device AC97 -device $(EDU)
Q35_TEST := INITRAMFS=$(INITRAMFS_TEST_IMAGE) CMDLINE='$(TEST_CMDLINE) saeos.test.pci=q35' ./scripts/run-qemu-test.sh
Q35_DEVICES := -machine q35 -device pci-bridge,id=br1,chassis_nr=1,bus=pcie.0 -device AC97,bus=br1 -device $(EDU),bus=pcie.0
boot-test: initramfs-test
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) --features $(BIOS_TEST_FEATURES),boot-test
	$(QEMU_TEST) $(BIOS_TEST_IMAGE) -m $(TEST_MEM) $(PCI_TEST_DEVICES)
	$(SMAP_EXPECT) $(QEMU_TEST) $(BIOS_TEST_IMAGE) -m $(TEST_MEM) -cpu max $(PCI_TEST_DEVICES)
	$(Q35_TEST) $(BIOS_TEST_IMAGE) -m $(TEST_MEM) $(Q35_DEVICES)
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) --features $(UEFI_TEST_FEATURES),boot-test
	$(QEMU_TEST) $(UEFI_TEST_IMAGE) -m $(TEST_MEM) $(PCI_TEST_DEVICES)
	$(SMAP_EXPECT) $(QEMU_TEST) $(UEFI_TEST_IMAGE) -m $(TEST_MEM) -cpu max $(PCI_TEST_DEVICES)
	$(Q35_TEST) $(UEFI_TEST_IMAGE) -m $(TEST_MEM) $(Q35_DEVICES)

mm-stress: initramfs-test
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) --features $(BIOS_TEST_FEATURES),boot-test,mm-stress,mm-allocator-stats,paging-debug
	INITRAMFS=$(INITRAMFS_TEST_IMAGE) CMDLINE='$(TEST_CMDLINE)' ./scripts/run-mm-stress.sh $(BIOS_TEST_IMAGE) -m 16G $(PCI_TEST_DEVICES)

mm-stress-uefi: initramfs-test
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) --features $(UEFI_TEST_FEATURES),boot-test,mm-stress,mm-allocator-stats,paging-debug
	INITRAMFS=$(INITRAMFS_TEST_IMAGE) CMDLINE='$(TEST_CMDLINE)' ./scripts/run-mm-stress.sh $(UEFI_TEST_IMAGE) -m 16G $(PCI_TEST_DEVICES)

# Userspace in ring 3: the test initramfs's usertest runs as init (PID 1)
# and its exit status is the verdict (saeos.test.exit, default 0). Then a
# user program that faults must be killed while the kernel survives, and a
# missing init must give the "No working init found" panic. The log markers
# matter as much as the exit status: if the syscall path itself broke, a
# failing program's exit(code) could arrive as exit(0).
USER_TEST_RUN = INITRAMFS=$(INITRAMFS_TEST_IMAGE) ./scripts/run-qemu-test.sh
user-test: initramfs-test
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) --features $(BIOS_TEST_FEATURES),user-test
	$(call user_tests,$(BIOS_TEST_IMAGE))
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) --features $(UEFI_TEST_FEATURES),user-test
	$(call user_tests,$(UEFI_TEST_IMAGE))

define user_tests
	CMDLINE='init=/test/bin/usertest' QEMU_TEST_EXPECT="$$(printf 'usertest: direction flag kept across interrupts\nusertest: PASS\nuser-mode preemptions: [1-9]')" $(USER_TEST_RUN) $(1) -m 2G
	CMDLINE='init=/test/bin/usertest saeos.test.exit=sig11 -- crash' QEMU_TEST_EXPECT="killed by SIGSEGV: PAGE FAULT at 0x0" $(USER_TEST_RUN) $(1) -m 2G
	CMDLINE='init=/test/bin/usertest saeos.test.exit=sig4 -- ud' QEMU_TEST_EXPECT="killed by SIGILL" $(USER_TEST_RUN) $(1) -m 2G
	CMDLINE='init=/missing saeos.test=no-init' QEMU_TEST_EXPECT="Requested init /missing failed \(error -2\)" $(USER_TEST_RUN) $(1) -m 2G
	CMDLINE='init=/test/bin/usertest -- tty' $(TTY_INPUT) QEMU_TEST_KEYS='$(TTY_KEYS)' QEMU_TEST_EXPECT="usertest: tty PASS" $(USER_TEST_RUN) $(1) -m 2G
	CMDLINE='init=/test/bin/usertest -- tty' $(TTY_INPUT) QEMU_TEST_SERIAL='$(TTY_SERIAL)' QEMU_TEST_EXPECT="usertest: tty PASS" $(USER_TEST_RUN) $(1) -m 2G
	CMDLINE='init=/test/bin/usertest -- shell' QEMU_TEST_EXPECT="usertest: shell PASS" $(USER_TEST_RUN) $(1) -m 2G
	CMDLINE='init=/bin/sh saeos.test.exit=5' QEMU_TEST_INPUT_AFTER='saeos:/$$ ' QEMU_TEST_KEYS='$(SHELL_KEYS)' QEMU_TEST_EXPECT="$$(printf '^typed-ok$$\n^/etc$$')" $(USER_TEST_RUN) $(1) -m 2G
endef

# Sound: usertest plays tones on each sound card while QEMU records what
# the card outputs; check-wav.py then requires each tone, in order, at its
# pitch, unbroken (see the script). A wrong sample rate shows as a wrong
# pitch, a lost period as a phase jump.
SOUND_TONES := 440:1 440:0.5 1000:0.5 880:0.15 550:0.15 660:0.5
SOUND_RUN = CMDLINE='init=/test/bin/usertest -- sound' QEMU_TEST_EXPECT="usertest: sound PASS" \
	QEMU_TEST_POSTCHECK="./scripts/check-wav.py $(2) $(SOUND_TONES)" $(USER_TEST_RUN) $(1) -m 2G \
	-audiodev wav,id=snd,path=$(2) $(3)
# Each card on both boot paths; HD Audio on q35 too (the ICH9 controller).
Q35_HDA_DEVICES = -machine q35 -device ich9-intel-hda -device hda-output$(COMMA)audiodev=snd
sound-test: initramfs-test
	./scripts/check-wav.py --self-test
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) --features $(BIOS_TEST_FEATURES),user-test
	$(call SOUND_RUN,$(BIOS_TEST_IMAGE),target/sound-bios-ac97.wav,$(AC97_DEVICES))
	$(call SOUND_RUN,$(BIOS_TEST_IMAGE),target/sound-bios-hda.wav,$(HDA_DEVICES))
	$(call SOUND_RUN,$(BIOS_TEST_IMAGE),target/sound-bios-q35-hda.wav,$(Q35_HDA_DEVICES))
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) --features $(UEFI_TEST_FEATURES),user-test
	$(call SOUND_RUN,$(UEFI_TEST_IMAGE),target/sound-uefi-ac97.wav,$(AC97_DEVICES))
	$(call SOUND_RUN,$(UEFI_TEST_IMAGE),target/sound-uefi-hda.wav,$(HDA_DEVICES))

# Framebuffer: usertest draws on /dev/fb0 (and a forked child through the
# inherited mapping, and a row with write()); QEMU screendumps taken at
# its markers must show that picture pixel for pixel, then the console
# back once it lets go (on BIOS: VGA text mode again, 720x400, with its
# font). BIOS gets Bochs VBE at exactly 960x720, UEFI the firmware's GOP
# mode.
FB_RUN = CMDLINE='init=/test/bin/usertest -- fb $(3)' QEMU_TEST_EXPECT="usertest: fb PASS" \
	QEMU_TEST_SCREENDUMPS="$$(printf 'usertest: fb ready|%s\nusertest: fb released|%s' $(abspath $(2))-drawn.ppm $(abspath $(2))-console.ppm)" \
	QEMU_TEST_POSTCHECK="./scripts/check-screen.py drawn $(2)-drawn.ppm $(3) && ./scripts/check-screen.py console $(2)-console.ppm $(4)" \
	$(USER_TEST_RUN) $(1) -m 2G
fb-test: initramfs-test
	./scripts/check-screen.py --self-test
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) --features $(BIOS_TEST_FEATURES),user-test
	$(call FB_RUN,$(BIOS_TEST_IMAGE),target/fb-bios,960x720,720x400)
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) --features $(UEFI_TEST_FEATURES),user-test
	$(call FB_RUN,$(UEFI_TEST_IMAGE),target/fb-uefi,)

# Bad Apple's player on a generated clip (a square moving for 2 s, then a
# known last frame, over a 440 Hz tone): every frame drawn, under 100 ms
# behind the sound (`--check`), the last frame on screen pixel for pixel,
# and the tone recorded whole.
BADAPPLE_RUN = CMDLINE='init=/bin/badapple -- /test/clip.bav /test/clip.wav --check' \
	QEMU_TEST_EXPECT="$$(printf 'badapple: 61 frames\nbadapple: done')" \
	QEMU_TEST_SCREENDUMPS="badapple: done|$(abspath $(2))-final.ppm" \
	QEMU_TEST_POSTCHECK="./scripts/check-screen.py bav-final $(2)-final.ppm && ./scripts/check-wav.py $(2).wav 440:2" \
	$(USER_TEST_RUN) $(1) -m 2G -audiodev wav,id=snd,path=$(2).wav $(AC97_DEVICES)
badapple-test: initramfs-test
	./scripts/bav.py --self-test
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) --features $(BIOS_TEST_FEATURES),user-test
	$(call BADAPPLE_RUN,$(BIOS_TEST_IMAGE),target/badapple-bios)
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) --features $(UEFI_TEST_FEATURES),user-test
	$(call BADAPPLE_RUN,$(UEFI_TEST_IMAGE),target/badapple-uefi)

# Bad Apple itself, from the original video (ffmpeg; not part of the
# tests): initramfs/badapple/ (git-ignored), played by `badapple`.
BAD_APPLE_SRC ?= ../bad-apple-terminal/data
badapple-data:
	./scripts/make-badapple.py $(BAD_APPLE_SRC) initramfs/badapple

# The interactive shell as init, typed at: "echo typed-ok", "cd /etc",
# "pwd", "exit 5" (the exit status is the verdict).
SHELL_KEYS := e c h o spc t y p e d minus o k ret c d spc slash e t c ret p w d ret e x i t spc 5 ret

# Console input, typed once usertest is reading: "hello tty", Enter,
# "abx", Backspace, "c", Enter, Ctrl-D; on the PS/2 keyboard, then on the
# serial port (CR for Enter, DEL for Backspace, as a terminal sends).
TTY_INPUT := QEMU_TEST_INPUT_AFTER="usertest: tty ready"
TTY_KEYS := h e l l o spc t t y ret a b x backspace c ret ctrl-d
TTY_SERIAL := hello tty\rabx\x7fc\r\x04

# A task executes ud2: the #UD report must name it.
fault-test: initramfs-test
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) --features $(BIOS_TEST_FEATURES),fault-test
	QEMU_TEST_SYMBOLS="fault_tests::fault_test_task" QEMU_TEST_EXPECT="INVALID OPCODE \(#UD\).*task 'fault-test'" $(QEMU_TEST) $(BIOS_TEST_IMAGE) -m $(TEST_MEM)
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) --features $(UEFI_TEST_FEATURES),fault-test
	QEMU_TEST_SYMBOLS="fault_tests::fault_test_task" QEMU_TEST_EXPECT="INVALID OPCODE \(#UD\).*task 'fault-test'" $(QEMU_TEST) $(UEFI_TEST_IMAGE) -m $(TEST_MEM)

# A task panics while holding the console and serial locks: the report must
# still come out instead of deadlocking.
panic-lock-test: initramfs-test
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) --features $(BIOS_TEST_FEATURES),panic-lock-test
	QEMU_TEST_SYMBOLS="fault_tests::panic_lock_test_task" QEMU_TEST_EXPECT="panic-lock-test: deliberate panic" $(QEMU_TEST) $(BIOS_TEST_IMAGE) -m $(TEST_MEM)
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) --features $(UEFI_TEST_FEATURES),panic-lock-test
	QEMU_TEST_SYMBOLS="fault_tests::panic_lock_test_task" QEMU_TEST_EXPECT="panic-lock-test: deliberate panic" $(QEMU_TEST) $(UEFI_TEST_IMAGE) -m $(TEST_MEM)

mm-guard-test: initramfs-test
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) --features $(BIOS_TEST_FEATURES),mm-guard-test
	QEMU_TEST_SYMBOLS="guard_test_task::recurse" QEMU_TEST_EXPECT="KERNEL STACK OVERFLOW in task 'guard-test'" $(QEMU_TEST) $(BIOS_TEST_IMAGE) -m $(TEST_MEM)
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) --features $(UEFI_TEST_FEATURES),mm-guard-test
	QEMU_TEST_SYMBOLS="guard_test_task::recurse" QEMU_TEST_EXPECT="KERNEL STACK OVERFLOW in task 'guard-test'" $(QEMU_TEST) $(UEFI_TEST_IMAGE) -m $(TEST_MEM)

fmt-check:
	cargo fmt --all -- --check

clippy: olddefconfig
	cargo clippy -p $(KERNEL) --target $(BIOS_TARGET) $(FEATURE_ARGS) -- -D warnings
	cargo clippy -p $(KERNEL) --target $(UEFI_TARGET) $(FEATURE_ARGS) -- -D warnings
	# The test builds compile code the configured build does not.
	cargo clippy -p $(KERNEL) --target $(BIOS_TARGET) --features $(BIOS_TEST_FEATURES),boot-test -- -D warnings
	cargo clippy -p $(KERNEL) --target $(UEFI_TARGET) --features $(UEFI_TEST_FEATURES),user-test -- -D warnings
	# And what they leave out: the code behind every optional feature off.
	cargo clippy -p $(KERNEL) --target $(BIOS_TARGET) --features $(TINY_FEATURES) -- -D warnings
	cargo clippy -p $(KERNEL) --target $(UEFI_TARGET) --features $(TINY_FEATURES) -- -D warnings
	cargo clippy -p ratconf -- -D warnings
	cargo clippy -p mkinitramfs --all-targets -- -D warnings
	cargo clippy -Zjson-target-spec -Zbuild-std=core,alloc --target $(USER_TARGET) -p libsaeos $(addprefix -p ,$(USER_PROGRAMS)) -- -D warnings

menuconfig:
	$(RATCONF) menuconfig

defconfig:
	$(RATCONF) defconfig

tinyconfig:
	$(RATCONF) tinyconfig

olddefconfig:
	$(RATCONF) olddefconfig

savedefconfig:
	$(RATCONF) savedefconfig

headers_check:
	@echo "headers_check: no exported userspace headers exist yet"

headers_install:
	@echo "headers_install: no exported userspace headers exist yet"

clean:
	cargo clean

distclean: clean
	rm -f .config .config.cargo.toml defconfig.toml

mrproper: distclean
	rm -rf result result-*

nix-gc:
	nix-collect-garbage -d

help:
	@echo "SaeOS build targets:"
	@echo "  all bios uefi asm asm-bios asm-uefi run run-uefi run-debug run-uefi-debug check"
	@echo "  run options: make run-uefi QEMU_ARGS=\"-m 16g\" CMDLINE=\"loglevel=debug\" SOUND=pipewire SOUND_CARD=hda"
	@echo "  userspace initramfs initramfs-test (from initramfs.toml)"
	@echo "  menuconfig defconfig tinyconfig olddefconfig savedefconfig"
	@echo "  test boot-test user-test sound-test fb-test badapple-test host-test miri mm-stress mm-stress-uefi mm-guard-test fault-test panic-lock-test"
	@echo "  fmt-check clippy"
	@echo "  debug debug-uefi gdb gdb-uefi symbolize LOG=<file>"
	@echo "  clean distclean mrproper nix-gc"
