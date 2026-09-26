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
