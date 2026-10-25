/* Layout of every SaeOS user program: a static, non-PIE ELF at 4 MiB,
 * like a classic Linux static binary, with exactly three segments:
 *
 *   0x400000  R    ELF and program headers (for AT_PHDR), .rodata
 *   next page R X  .text
 *   next page RW   .data, .tdata/.tbss (the TLS template, also described
 *                  by PT_TLS for the runtime to copy per thread), .bss
 *
 * Each segment starts on its own page, so no page is ever both writable
 * and executable, and no two segments share a page. */
ENTRY(_start)

PHDRS
{
    headers PT_PHDR PHDRS;
    rodata  PT_LOAD FILEHDR PHDRS;
    text    PT_LOAD;
    data    PT_LOAD;
    tls     PT_TLS;
}

SECTIONS
{
    . = 0x400000 + SIZEOF_HEADERS;
    .rodata : { *(.rodata .rodata.*) } :rodata

    . = ALIGN(4K);
    .text : { *(.text .text.*) } :text

    . = ALIGN(4K);
    .data : { *(.data .data.*) *(.got .got.*) } :data
    .tdata : { *(.tdata .tdata.*) } :data :tls
    .tbss  : { *(.tbss .tbss.*) } :data :tls
    .bss  : { *(.bss .bss.*) *(COMMON) } :data

    /* panic=abort: no unwinding tables needed. */
    /DISCARD/ : { *(.eh_frame*) *(.note .note.*) *(.comment) }
}
