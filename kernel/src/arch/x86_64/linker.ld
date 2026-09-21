ENTRY(_start)

SECTIONS {
	. = 1M;
	_kernel_start = .;

	.boot :
	{
		KEEP(*(.multiboot))
	}

	.text : ALIGN(4K)
	{
		_text_start = .;
		*(.text .text.*)
		_text_end = .;
	}

	.rodata : ALIGN(4K)
	{
		_rodata_start = .;
		*(.rodata .rodata.*)
		_rodata_end = .;
	}

	.data : ALIGN(4K)
	{
		_data_start = .;
		*(.data .data.*)
		_data_end = .;
	}

	.bss : ALIGN(4K)
	{
		_bss_start = .;
		*(COMMON)
		*(.bss .bss.*)
		_bss_end = .;
	}

	. = ALIGN(4K);
	_kernel_end = .;

	/DISCARD/ :
	{
		*(.comment)
		*(.eh_frame)
		*(.note.*)
	}
}
