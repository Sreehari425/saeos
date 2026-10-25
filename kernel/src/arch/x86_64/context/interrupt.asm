bits 64
default rel

section .text

extern saeos_timer_interrupt_rust

; Timer IRQ entry. The CPU has already pushed the kernel interrupt return
; frame. Save all general-purpose registers above it, then let Rust return the
; stack pointer of the task whose frame should resume.
global saeos_timer_interrupt_entry
saeos_timer_interrupt_entry:
    push rax
    push rbx
    push rcx
    push rdx
    push rbp
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    ; Interrupts can arrive at any point in a kernel function, so the
    ; interrupted RSP does not guarantee the SysV call-site alignment.
    ; Preserve the saved-frame pointer in RDI and align the callback stack
    ; independently below the saved registers.
    mov rdi, rsp
    and rsp, -16
    ; The CPU does not clear DF on an interrupt, and user code may be in the
    ; middle of a backwards copy (std; rep movsb). Rust assumes DF=0 at
    ; every call, so its memcpy would run backwards and corrupt memory.
    ; iretq restores the interrupted RFLAGS, DF included.
    cld
    call saeos_timer_interrupt_rust
    ; RAX is the selected context pointer. RDX distinguishes an interrupt
    ; frame from a saved voluntary context.
    test rdx, rdx
    jnz .restore_voluntary
    mov rsp, rax
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rbp
    pop rdx
    pop rcx
    pop rbx
    pop rax
    iretq

.restore_voluntary:
    mov rsi, rax
    mov rax, [rsi + 64]
    btr rax, 9
    push rax
    popfq
    mov rsp, [rsi + 0]
    mov rbx, [rsi + 16]
    mov rbp, [rsi + 24]
    mov r12, [rsi + 32]
    mov r13, [rsi + 40]
    mov r14, [rsi + 48]
    mov r15, [rsi + 56]
    mov r11, [rsi + 8]
    ; The saved call-boundary continuation resumes after its assembly call.
    ; RAX is caller-saved for ordinary switches.
    mov eax, 1
    jmp r11

; Start a task from a prepared kernel-mode interrupt frame.
global saeos_start_interrupt_context
saeos_start_interrupt_context:
    mov rsp, rdi
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rbp
    pop rdx
    pop rcx
    pop rbx
    pop rax
    iretq

extern saeos_page_fault_rust

; Page fault (#PF) entry. The CPU pushed the error code above its return
; frame. Save every general-purpose register (same order as the timer stub)
; and hand Rust a pointer to the whole frame, so it can resume a faulting
; user-memory copy at its fixup label by rewriting the saved RIP.
global saeos_page_fault_entry
saeos_page_fault_entry:
    push rax
    push rbx
    push rcx
    push rdx
    push rbp
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    mov rdi, rsp
    ; RBX is saved above, so it can hold the unaligned stack pointer.
    mov rbx, rsp
%ifidn __OUTPUT_FORMAT__, elf64
    ; BIOS backtraces walk frame pointers: add a frame record whose return
    ; address is the faulting RIP (above the 15 saved registers and the
    ; error code), so the function that faulted is shown. The UEFI build
    ; unwinds with .pdata and finds it on its own.
    push qword [rsp + 16 * 8]
    push rbp
    mov rbp, rsp
%endif
    and rsp, -16
    ; DF=0 for Rust, as in the timer stub: a backwards user copy that hits
    ; a copy-on-write page faults with DF set.
    cld
    call saeos_page_fault_rust
    mov rsp, rbx
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rbp
    pop rdx
    pop rcx
    pop rbx
    pop rax
    ; Drop the error code.
    add rsp, 8
    iretq
