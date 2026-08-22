global _start
extern kernel_main

section .multiboot
align 4
    dd 0x1BADB002                 ; Multiboot 1 magic
    dd 0x00000003                 ; Flags: ALIGN | MEMINFO
    dd -(0x1BADB002 + 0x00000003) ; Checksum

section .bss
align 4096
p4_table:
    resb 4096
p3_table:
    resb 4096
p2_table:
    resb 4096 * 4                 ; 4 Page Directories to map 4 GiB
stack_bottom:
    resb 65536                    ; 64 KB stack
stack_top:

section .rodata
gdt64:
    dq 0                          ; Null descriptor
.code: equ $ - gdt64
    dq (1<<43) | (1<<44) | (1<<47) | (1<<53) ; 64-bit code segment: executable, code, present, 64-bit
.pointer:
    dw $ - gdt64 - 1
    dq gdt64

section .text
bits 32
_start:
    mov esp, stack_top
    mov edi, ebx                 ; Save Multiboot info pointer in edi (becomes rdi in 64-bit)

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
    ; Reload data segment registers
    mov ax, 0
    mov ss, ax
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax

    ; Set 64-bit stack
    mov rsp, stack_top

    ; Call 64-bit Rust kernel entry point
    call kernel_main

    ; If kernel returns, halt
    cli
.hlt_loop:
    hlt
    jmp .hlt_loop

section .note.GNU-stack noalloc noexec nowrite progbits
