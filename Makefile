RATCONF := cargo run --quiet -p ratconf --
KERNEL := saeos
BIOS_TARGET := x86_64-unknown-none
UEFI_TARGET := x86_64-unknown-uefi
QEMU_ARGS ?=
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
TEST_FEATURES := serial,apic,apic-timer,pit,tsc,keyboard
BIOS_TEST_FEATURES := $(TEST_FEATURES),vga-text
UEFI_TEST_FEATURES := $(TEST_FEATURES),framebuffer
# Enough RAM to exercise frames above 4 GiB.
TEST_MEM ?= 6G
# The tests boot the image with /test fixtures and check this marker.
TEST_CMDLINE := loglevel=info saeos.test=boot
QEMU_TEST := INITRAMFS=$(INITRAMFS_TEST_IMAGE) CMDLINE='$(TEST_CMDLINE)' ./scripts/run-qemu-test.sh
BIOS_TEST_IMAGE := ./scripts/runner-bios-x86_64.sh target/$(BIOS_TARGET)/debug/$(KERNEL)
UEFI_TEST_IMAGE := ./scripts/runner-uefi-x86_64.sh target/$(UEFI_TARGET)/debug/$(KERNEL).efi

.PHONY: check-std-lock userspace initramfs initramfs-test all bios uefi asm asm-bios asm-uefi run run-uefi run-debug run-uefi-debug debug debug-uefi gdb gdb-uefi symbolize check menuconfig defconfig tinyconfig olddefconfig savedefconfig
.PHONY: headers_check headers_install clean distclean mrproper nix-gc fmt-check clippy mm-test miri mm-stress mm-stress-uefi mm-guard-test fault-test panic-lock-test boot-test user-test test help

all: bios

# User programs, built for the SaeOS target (userspace/x86_64-saeos.json:
# static, SSE, linked with userspace/user.ld) with core and alloc built from
# source; installed by initramfs.toml. Each must be a static executable
# whose entry point is in its executable segment.
USER_PROGRAMS := hello usertest
USER_TARGET := userspace/x86_64-saeos.json
USER_BUILD = cargo build --release -Zjson-target-spec \
	-Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem \
	--target $(USER_TARGET)
userspace:
	$(USER_BUILD) $(addprefix -p ,$(USER_PROGRAMS))
	@for program in $(USER_PROGRAMS); do \
		./scripts/check-user-elf.sh target/x86_64-saeos/release/$$program || exit 1; \
	done

# The normal image must never carry the test fixtures.
initramfs: userspace
	$(MKINITRAMFS) build initramfs.toml -o $(INITRAMFS_IMAGE)
	@if $(MKINITRAMFS) list $(INITRAMFS_IMAGE) | grep -q ' /test/'; then \
		echo "initramfs: $(INITRAMFS_IMAGE) contains /test" >&2; exit 1; fi

initramfs-test: userspace
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
	CMDLINE='$(CMDLINE)' nix run path:.#bios -- $(QEMU_ARGS)

run-uefi: olddefconfig
	CMDLINE='$(CMDLINE)' nix run path:.#uefi -- $(QEMU_ARGS)

run-debug: olddefconfig initramfs
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) $(FEATURE_ARGS)
	$(BOOT_ENV) ./scripts/runner-bios-x86_64.sh target/$(BIOS_TARGET)/debug/$(KERNEL) $(QEMU_ARGS)

run-uefi-debug: olddefconfig initramfs
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) $(FEATURE_ARGS)
	$(BOOT_ENV) ./scripts/runner-uefi-x86_64.sh target/$(UEFI_TARGET)/debug/$(KERNEL).efi $(QEMU_ARGS)

# Start QEMU halted with a GDB stub on :1234, then attach with `make gdb`
# (or `make gdb-uefi`) from a second terminal.
debug: olddefconfig initramfs
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) $(FEATURE_ARGS)
	$(BOOT_ENV) ./scripts/runner-bios-x86_64.sh target/$(BIOS_TARGET)/debug/$(KERNEL) -s -S -no-shutdown $(QEMU_ARGS)

debug-uefi: olddefconfig initramfs
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) $(FEATURE_ARGS)
	$(BOOT_ENV) ./scripts/runner-uefi-x86_64.sh target/$(UEFI_TARGET)/debug/$(KERNEL).efi -s -S -no-shutdown $(QEMU_ARGS)

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
	cargo check -p mkinitramfs
	cargo check -Zjson-target-spec -Zbuild-std=core,alloc --target $(USER_TARGET) -p saeos-rt $(addprefix -p ,$(USER_PROGRAMS))

# The Nix build vendors the standard library's dependencies from this copy;
# refresh it with the toolchain: cp "$$(rustc --print sysroot)/lib/rustlib/src/rust/library/Cargo.lock" userspace/std-library.Cargo.lock
check-std-lock:
	@cmp -s userspace/std-library.Cargo.lock "$$(rustc --print sysroot)/lib/rustlib/src/rust/library/Cargo.lock" \
		|| { echo "userspace/std-library.Cargo.lock is stale for this toolchain" >&2; exit 1; }

mm-test:
	cargo test -p mm-model
	cargo test -p mkinitramfs

# The host model (buddy core, scheduler, task code, log ring) under Miri,
# which checks for undefined behaviour the normal tests cannot see.
miri:
	cargo miri test -p mm-model

# Host tests plus every QEMU test on both boot paths; each run ends with the
# kernel's own verdict through isa-debug-exit.
test: mm-test miri boot-test user-test mm-guard-test fault-test panic-lock-test

# Each target boots twice: on QEMU's default CPU, and with -cpu max, where
# SMEP/SMAP exist and the user-memory test checks they are enforced.
SMAP_EXPECT := QEMU_TEST_EXPECT="uaccess smap=true smep=true"
boot-test: initramfs-test
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) --features $(BIOS_TEST_FEATURES),boot-test
	$(QEMU_TEST) $(BIOS_TEST_IMAGE) -m $(TEST_MEM)
	$(SMAP_EXPECT) $(QEMU_TEST) $(BIOS_TEST_IMAGE) -m $(TEST_MEM) -cpu max
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) --features $(UEFI_TEST_FEATURES),boot-test
	$(QEMU_TEST) $(UEFI_TEST_IMAGE) -m $(TEST_MEM)
	$(SMAP_EXPECT) $(QEMU_TEST) $(UEFI_TEST_IMAGE) -m $(TEST_MEM) -cpu max

mm-stress: initramfs-test
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) --features $(BIOS_TEST_FEATURES),boot-test,mm-stress,mm-allocator-stats,paging-debug
	INITRAMFS=$(INITRAMFS_TEST_IMAGE) CMDLINE='$(TEST_CMDLINE)' ./scripts/run-mm-stress.sh $(BIOS_TEST_IMAGE) -m 16G

mm-stress-uefi: initramfs-test
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) --features $(UEFI_TEST_FEATURES),boot-test,mm-stress,mm-allocator-stats,paging-debug
	INITRAMFS=$(INITRAMFS_TEST_IMAGE) CMDLINE='$(TEST_CMDLINE)' ./scripts/run-mm-stress.sh $(UEFI_TEST_IMAGE) -m 16G

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
	CMDLINE='init=/test/bin/usertest' QEMU_TEST_EXPECT="$$(printf 'usertest: PASS\nuser-mode preemptions: [1-9]')" $(USER_TEST_RUN) $(1) -m 2G
	CMDLINE='init=/test/bin/usertest saeos.test.exit=sig11 -- crash' QEMU_TEST_EXPECT="killed by SIGSEGV: PAGE FAULT at 0x0" $(USER_TEST_RUN) $(1) -m 2G
	CMDLINE='init=/test/bin/usertest saeos.test.exit=sig4 -- ud' QEMU_TEST_EXPECT="killed by SIGILL" $(USER_TEST_RUN) $(1) -m 2G
	CMDLINE='init=/missing saeos.test=no-init' QEMU_TEST_EXPECT="Requested init /missing failed \(error -2\)" $(USER_TEST_RUN) $(1) -m 2G
endef

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
	cargo clippy -p ratconf -- -D warnings
	cargo clippy -p mkinitramfs --all-targets -- -D warnings
	cargo clippy -Zjson-target-spec -Zbuild-std=core,alloc --target $(USER_TARGET) -p saeos-rt $(addprefix -p ,$(USER_PROGRAMS)) -- -D warnings

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
	@echo "  run options: make run-uefi QEMU_ARGS=\"-m 16g\" CMDLINE=\"loglevel=debug\""
	@echo "  userspace initramfs initramfs-test (from initramfs.toml)"
	@echo "  menuconfig defconfig tinyconfig olddefconfig savedefconfig"
	@echo "  test boot-test user-test mm-test miri mm-stress mm-stress-uefi mm-guard-test fault-test panic-lock-test"
	@echo "  fmt-check clippy"
	@echo "  debug debug-uefi gdb gdb-uefi symbolize LOG=<file>"
	@echo "  clean distclean mrproper nix-gc"
