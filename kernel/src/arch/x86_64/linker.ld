ENTRY(_start)

/* Must match KERNEL_OFFSET in boot.asm and mm::layout::KERNEL_IMAGE_BASE. */
KERNEL_OFFSET = 0xffffffff80000000;

SECTIONS {
	. = 1M;
	_kernel_start = .;

	/* Low boot stub: runs before paging, so VMA == LMA. */
	.boot :
	{
		KEEP(*(.multiboot))
	}

	.boot.text : ALIGN(16)
	{
		*(.boot.text)
	}

	.boot.rodata : ALIGN(16)
	{
		*(.boot.rodata)
	}

	.boot.bss (NOLOAD) : ALIGN(4K)
	{
		_boot_bss_start = .;
		*(.boot.bss)
		_boot_bss_end = .;
	}

	/* Everything else runs from the top-2-GiB window at KERNEL_OFFSET + LMA. */
	. = ALIGN(4K) + KERNEL_OFFSET;

	.text : AT(ADDR(.text) - KERNEL_OFFSET) ALIGN(4K)
	{
		_text_start = .;
		*(.text .text.*)
		_text_end = .;
	}

	.rodata : AT(ADDR(.rodata) - KERNEL_OFFSET) ALIGN(4K)
	{
		_rodata_start = .;
		*(.rodata .rodata.*)
		_rodata_end = .;
	}

	.data : AT(ADDR(.data) - KERNEL_OFFSET) ALIGN(4K)
	{
		_data_start = .;
		*(.data .data.*)
		*(.got .got.*)
		_data_end = .;
	}

	.bss : AT(ADDR(.bss) - KERNEL_OFFSET) ALIGN(4K)
	{
		_bss_start = .;
		*(COMMON)
		*(.bss .bss.*)
		_bss_end = .;
	}

	. = ALIGN(4K);
	_kernel_end = .;

	/* Physical .bss bounds for the pre-paging clear in boot.asm. */
	_bss_start_phys = _bss_start - KERNEL_OFFSET;
	_bss_end_phys = _bss_end - KERNEL_OFFSET;

	/DISCARD/ :
	{
		*(.comment)
		*(.eh_frame)
		*(.note.*)
	}
}
