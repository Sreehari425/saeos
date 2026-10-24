global _start
extern kernel_main_bios
extern _boot_bss_start
extern _boot_bss_end
extern _bss_start_phys
extern _bss_end_phys

; Must match KERNEL_OFFSET in linker.ld and mm::layout::KERNEL_IMAGE_BASE.
; The low stub below runs at its load address; everything in .text/.rodata/
; .data/.bss is linked at KERNEL_OFFSET + load address.
KERNEL_OFFSET equ 0xffffffff80000000

section .multiboot
align 4
    dd 0x1BADB002                 ; Multiboot 1 magic
    dd 0x00000043                 ; Flags: ALIGN | MEMINFO | MEMORY MAP
    dd -(0x1BADB002 + 0x00000043) ; Checksum

section .boot.bss nobits alloc write align=4096
align 4096
p4_table:
    resb 4096
p3_table:
    resb 4096
p3_high_table:
    resb 4096
p2_table:
    resb 4096 * 4                 ; 4 Page Directories to map 4 GiB
boot_stack_bottom:
    resb 16384                    ; 32-bit stack used before the high jump
boot_stack_top:

section .bss
align 4096
stack_bottom:
    resb 524288                   ; 512 KB stack (mapping-plan diagnostics need ~128 KB)
stack_top:

section .boot.rodata progbits alloc noexec nowrite align=16
gdt64:
    dq 0                          ; Null descriptor
.code: equ $ - gdt64
    dq (1<<43) | (1<<44) | (1<<47) | (1<<53) ; 64-bit code segment: executable, code, present, 64-bit
.pointer:
    dw $ - gdt64 - 1
    dq gdt64

section .boot.text progbits alloc exec nowrite align=16
bits 32
_start:
    mov esi, ebx                 ; Preserve Multiboot info while clearing .bss
    mov ebp, eax                 ; Preserve Multiboot magic while clearing .bss

    ; BIOS/Multiboot does not guarantee that .bss is zeroed. Rust statics
    ; (including spinlocks and page-table state) require it. Paging is off,
    ; so both ranges are cleared through their physical addresses.
    mov edi, _boot_bss_start
    mov ecx, _boot_bss_end
    sub ecx, edi
    xor eax, eax
    rep stosb
    mov edi, _bss_start_phys
    mov ecx, _bss_end_phys
    sub ecx, edi
    rep stosb

    mov esp, boot_stack_top
    mov edi, esi                 ; Save Multiboot info pointer in edi (becomes rdi in 64-bit)
    mov eax, ebp                 ; Restore Multiboot magic

    ; Check Multiboot magic
    cmp eax, 0x2BADB002
    jne .no_multiboot

    ; Check CPUID and Long Mode
    call check_cpuid
    call check_long_mode

    ; Set up 4-level paging (PML4 -> PDPT -> PD)
    call set_up_page_tables
    call enable_paging

    ; Load 64-bit GDT
    lgdt [gdt64.pointer]

    ; Far jump to 64-bit mode
    jmp gdt64.code:long_mode_start

.no_multiboot:
    mov al, "M"
    jmp error
.no_cpuid:
    mov al, "C"
    jmp error
.no_long_mode:
    mov al, "L"
    jmp error

error:
    ; Print "ERR: X" to VGA screen (0xb8000)
    mov dword [0xb8000], 0x4f524f45 ; 'E', 'R'
    mov dword [0xb8004], 0x4f3a4f52 ; 'R', ':'
    mov byte  [0xb8008], al
    mov byte  [0xb8009], 0x4f
    hlt

check_cpuid:
    pushfd
    pop eax
    mov ecx, eax
    xor eax, 1 << 21
    push eax
    popfd
    pushfd
    pop eax
    push ecx
    popfd
    cmp eax, ecx
    je .no_cpuid
    ret
.no_cpuid:
    jmp _start.no_cpuid

check_long_mode:
    mov eax, 0x80000000
    cpuid
    cmp eax, 0x80000001
    jb .no_long_mode

    mov eax, 0x80000001
    cpuid
    test edx, 1 << 29
    jz .no_long_mode
    ret
.no_long_mode:
    jmp _start.no_long_mode

set_up_page_tables:
    ; Map first P4 entry to P3 table
    mov eax, p3_table
    or eax, 0b11 ; present + writable
    mov [p4_table], eax
    mov dword [p4_table + 4], 0

    ; Map P3 entries 0..3 to the 4 P2 tables
    mov ecx, 0
.map_p3_table:
    mov eax, 4096
    mul ecx
    add eax, p2_table
    or eax, 0b11 ; present + writable
    mov [p3_table + ecx * 8], eax
    mov dword [p3_table + ecx * 8 + 4], 0
    inc ecx
    cmp ecx, 4
    jne .map_p3_table

    ; Alias the first 2 GiB at KERNEL_OFFSET: P4[511] -> P3_high, whose
    ; entries 510 and 511 reuse the first two identity page directories.
    mov eax, p3_high_table
    or eax, 0b11
    mov [p4_table + 511 * 8], eax
    mov dword [p4_table + 511 * 8 + 4], 0
    mov eax, p2_table
    or eax, 0b11
    mov [p3_high_table + 510 * 8], eax
    mov dword [p3_high_table + 510 * 8 + 4], 0
    add eax, 4096
    mov [p3_high_table + 511 * 8], eax
    mov dword [p3_high_table + 511 * 8 + 4], 0

    ; Map each P2 entry to a 2MiB huge page (maps 2048 * 2MiB = 4GiB total)
    mov ecx, 0
.map_p2_table:
    mov eax, 0x200000       ; 2MiB
    mul ecx                 ; address of ecx-th page (eax = low 32b, edx = high 32b)
    or eax, 0b10000011      ; present + writable + huge (2MiB)
    mov [p2_table + ecx * 8], eax
    mov [p2_table + ecx * 8 + 4], edx
    inc ecx
    cmp ecx, 2048
    jne .map_p2_table
    ret

enable_paging:
    ; Load P4 address into CR3
    mov eax, p4_table
    mov cr3, eax

    ; Enable PAE in CR4
    mov eax, cr4
    or eax, 1 << 5
    mov cr4, eax

    ; Set Long Mode bit in EFER MSR
    mov ecx, 0xC0000080
    rdmsr
    or eax, 1 << 8
    wrmsr

    ; Enable Paging in CR0
    mov eax, cr0
    or eax, 1 << 31
    mov cr0, eax
    ret

bits 64
long_mode_start:
    ; Still executing at the low load address; continue in the high window.
    mov rax, qword long_mode_high
    jmp rax

section .text
bits 64
long_mode_high:
    ; Reload data segment registers
    mov ax, 0
    mov ss, ax
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax

    ; Set 64-bit stack
    mov rsp, stack_top

    ; Upper halves are undefined after the mode switch; the Multiboot info
    ; pointer is a 32-bit physical address.
    mov edi, edi

    ; Call 64-bit Rust kernel entry point
    call kernel_main_bios

    ; If kernel returns, halt
    cli
.hlt_loop:
    hlt
    jmp .hlt_loop

section .note.GNU-stack noalloc noexec nowrite progbits
