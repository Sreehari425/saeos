RATCONF := cargo run --quiet -p ratconf --
KERNEL := saeos
BIOS_TARGET := x86_64-unknown-none
UEFI_TARGET := x86_64-unknown-uefi
QEMU_ARGS ?=
FEATURES = $(shell if [ -f .config ]; then cargo run --quiet -p ratconf -- features 2>/dev/null; fi)
FEATURE_ARGS = $(if $(FEATURES),--features $(FEATURES),)

# Fixed feature sets for the automated QEMU tests, independent of .config.
TEST_FEATURES := serial,apic,apic-timer,pit,tsc,keyboard
BIOS_TEST_FEATURES := $(TEST_FEATURES),vga-text
UEFI_TEST_FEATURES := $(TEST_FEATURES),framebuffer
# Enough RAM to exercise frames above 4 GiB.
TEST_MEM ?= 6G
BIOS_TEST_IMAGE := ./scripts/runner-bios-x86_64.sh target/$(BIOS_TARGET)/debug/$(KERNEL)
UEFI_TEST_IMAGE := ./scripts/runner-uefi-x86_64.sh target/$(UEFI_TARGET)/debug/$(KERNEL).efi

.PHONY: all bios uefi asm asm-bios asm-uefi run run-uefi run-debug run-uefi-debug check menuconfig defconfig tinyconfig olddefconfig savedefconfig
.PHONY: headers_check headers_install clean distclean mrproper nix-gc fmt-check clippy mm-test mm-stress mm-stress-uefi mm-guard-test fault-test panic-lock-test boot-test test help

all: bios

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
	nix run path:.#bios -- $(QEMU_ARGS)

run-uefi: olddefconfig
	nix run path:.#uefi -- $(QEMU_ARGS)

run-debug: olddefconfig
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) $(FEATURE_ARGS)
	./scripts/runner-bios-x86_64.sh target/$(BIOS_TARGET)/debug/$(KERNEL) $(QEMU_ARGS)

run-uefi-debug: olddefconfig
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) $(FEATURE_ARGS)
	./scripts/runner-uefi-x86_64.sh target/$(UEFI_TARGET)/debug/$(KERNEL).efi $(QEMU_ARGS)

check: olddefconfig
	cargo check -p $(KERNEL) --target $(BIOS_TARGET) $(FEATURE_ARGS)
	cargo check -p $(KERNEL) --target $(UEFI_TARGET) $(FEATURE_ARGS)
	cargo check -p ratconf

mm-test:
	cargo test -p mm-model

# Host tests plus every QEMU test on both boot paths; each run ends with the
# kernel's own verdict through isa-debug-exit.
test: mm-test boot-test mm-guard-test fault-test panic-lock-test

boot-test:
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) --features $(BIOS_TEST_FEATURES),boot-test
	./scripts/run-qemu-test.sh $(BIOS_TEST_IMAGE) -m $(TEST_MEM)
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) --features $(UEFI_TEST_FEATURES),boot-test
	./scripts/run-qemu-test.sh $(UEFI_TEST_IMAGE) -m $(TEST_MEM)

mm-stress:
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) --features $(BIOS_TEST_FEATURES),boot-test,mm-stress,mm-allocator-stats,paging-debug
	./scripts/run-mm-stress.sh $(BIOS_TEST_IMAGE) -m 16G

mm-stress-uefi:
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) --features $(UEFI_TEST_FEATURES),boot-test,mm-stress,mm-allocator-stats,paging-debug
	./scripts/run-mm-stress.sh $(UEFI_TEST_IMAGE) -m 16G

# A task executes ud2: the #UD report must name it.
fault-test:
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) --features $(BIOS_TEST_FEATURES),fault-test
	QEMU_TEST_EXPECT="INVALID OPCODE \(#UD\).*task 'fault-test'" ./scripts/run-qemu-test.sh $(BIOS_TEST_IMAGE) -m $(TEST_MEM)
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) --features $(UEFI_TEST_FEATURES),fault-test
	QEMU_TEST_EXPECT="INVALID OPCODE \(#UD\).*task 'fault-test'" ./scripts/run-qemu-test.sh $(UEFI_TEST_IMAGE) -m $(TEST_MEM)

# A task panics while holding the console and serial locks: the report must
# still come out instead of deadlocking.
panic-lock-test:
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) --features $(BIOS_TEST_FEATURES),panic-lock-test
	QEMU_TEST_EXPECT="panic-lock-test: deliberate panic" ./scripts/run-qemu-test.sh $(BIOS_TEST_IMAGE) -m $(TEST_MEM)
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) --features $(UEFI_TEST_FEATURES),panic-lock-test
	QEMU_TEST_EXPECT="panic-lock-test: deliberate panic" ./scripts/run-qemu-test.sh $(UEFI_TEST_IMAGE) -m $(TEST_MEM)

mm-guard-test:
	cargo build -p $(KERNEL) --target $(BIOS_TARGET) --features $(BIOS_TEST_FEATURES),mm-guard-test
	QEMU_TEST_EXPECT="KERNEL STACK OVERFLOW in task 'guard-test'" ./scripts/run-qemu-test.sh $(BIOS_TEST_IMAGE) -m $(TEST_MEM)
	cargo build -p $(KERNEL) --target $(UEFI_TARGET) --features $(UEFI_TEST_FEATURES),mm-guard-test
	QEMU_TEST_EXPECT="KERNEL STACK OVERFLOW in task 'guard-test'" ./scripts/run-qemu-test.sh $(UEFI_TEST_IMAGE) -m $(TEST_MEM)

fmt-check:
	cargo fmt --all -- --check

clippy: olddefconfig
	cargo clippy -p $(KERNEL) --target $(BIOS_TARGET) $(FEATURE_ARGS) -- -D warnings
	cargo clippy -p $(KERNEL) --target $(UEFI_TARGET) $(FEATURE_ARGS) -- -D warnings
	cargo clippy -p ratconf -- -D warnings

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
	@echo "  run options: make run-uefi QEMU_ARGS=\"-m 16g\""
	@echo "  menuconfig defconfig tinyconfig olddefconfig savedefconfig"
	@echo "  test boot-test mm-test mm-stress mm-stress-uefi mm-guard-test fault-test panic-lock-test"
	@echo "  fmt-check clippy"
	@echo "  clean distclean mrproper nix-gc"
