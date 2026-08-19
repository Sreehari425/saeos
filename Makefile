SRC_DIR := src
BIN_DIR := bin

ASM_SRC := $(SRC_DIR)/boot.asm
RUST_SRC := $(SRC_DIR)/main.rs
LINKER_SCRIPT := $(SRC_DIR)/linker.ld

BOOT_OBJ := $(BIN_DIR)/boot.o
KERNEL_OBJ := $(BIN_DIR)/kernel.o
KERNEL_ELF := $(BIN_DIR)/kernel64.elf
KERNEL_BIN := $(BIN_DIR)/saeos.bin

AS := nasm
LD := ld
RUSTC := rustc
OBJCOPY := objcopy
QEMU := qemu-system-x86_64

ASFLAGS := -f elf64
LDFLAGS := -m elf_x86_64 -n --no-warn-rwx-segments -nostdlib -T $(LINKER_SCRIPT)
RUSTFLAGS := --target x86_64-unknown-none --crate-type staticlib --emit=obj \
             -C opt-level=2 -C panic=abort -C relocation-model=static

.PHONY: all
all: $(KERNEL_BIN)

$(BIN_DIR):
	mkdir -p $(BIN_DIR)

# 1. Assemble the 64-bit Long Mode bootstrap
$(BOOT_OBJ): $(ASM_SRC) | $(BIN_DIR)
	$(AS) $(ASFLAGS) $< -o $@

# 2. Compile the Rust kernel for x86_64 bare metal
$(KERNEL_OBJ): $(RUST_SRC) | $(BIN_DIR)
	$(RUSTC) $(RUSTFLAGS) -o $@ $<

# 3. Link objects into 64-bit ELF
$(KERNEL_ELF): $(BOOT_OBJ) $(KERNEL_OBJ) $(LINKER_SCRIPT)
	$(LD) $(LDFLAGS) -o $@ $(BOOT_OBJ) $(KERNEL_OBJ)

# 4. Generate Multiboot-compatible kernel image for QEMU / GRUB
$(KERNEL_BIN): $(KERNEL_ELF)
	$(OBJCOPY) -O elf32-i386 $< $@

.PHONY: run
run: $(KERNEL_BIN)
	$(QEMU) -kernel $(KERNEL_BIN) -serial stdio

.PHONY: run-nox
run-nox: $(KERNEL_BIN)
	$(QEMU) -kernel $(KERNEL_BIN) -display none -serial stdio

.PHONY: clean
clean:
	rm -rf $(BIN_DIR)

.PHONY: rebuild
rebuild: clean all

.PHONY: help
help:
	@echo "SaeOS (x86_64) Build System"
	@echo "==========================="
	@echo "make         - Build the x86_64 kernel binary"
	@echo "make run     - Run kernel in QEMU (with GUI display and serial stdio)"
	@echo "make run-nox - Run kernel in QEMU headlessly (serial stdio only)"
	@echo "make clean   - Remove build artifacts"
	@echo "make rebuild - Clean and rebuild"
