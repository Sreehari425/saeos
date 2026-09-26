bits 64
default rel

section .text

extern saeos_task_entry_trampoline

global saeos_switch_context
saeos_switch_context:
    mov [rdi + 0], rsp
    lea rax, [rel .context_resume]
    mov [rdi + 8], rax
    mov [rdi + 16], rbx
    mov [rdi + 24], rbp
    mov [rdi + 32], r12
    mov [rdi + 40], r13
    mov [rdi + 48], r14
    mov [rdi + 56], r15
    pushfq
    pop rax
    mov [rdi + 64], rax

    ; Restore arithmetic/status flags while keeping IF clear. The Rust
    ; dispatch wrapper reenables interrupts only after the resumed call has
    ; finished restoring its caller state.
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
    ; A fresh task enables interrupts in saeos_task_bootstrap.
    mov rax, [rsi + 8]
    jmp rax

.context_resume:
    ret

global saeos_switch_to_interrupt_context
saeos_switch_to_interrupt_context:
    ; Save a normal call-boundary continuation in the outgoing task without
    ; inspecting the Rust wrapper's stack layout.
    mov [rdi + 0], rsp
    lea rax, [rel .interrupt_context_resume]
    mov [rdi + 8], rax
    mov [rdi + 16], rbx
    mov [rdi + 24], rbp
    mov [rdi + 32], r12
    mov [rdi + 40], r13
    mov [rdi + 48], r14
    mov [rdi + 56], r15
    pushfq
    pop rax
    mov [rdi + 64], rax

    ; Switch directly to the selected task's saved interrupt frame. The
    ; outgoing call continuation remains on its own stack until it resumes.
    mov rsp, rsi
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

.interrupt_context_resume:
    ret

; A fresh kernel task starts with its entry address in r12, a callee-saved
; register that is already part of VoluntaryContext.
global saeos_task_bootstrap
saeos_task_bootstrap:
    sti
    mov rdi, r12
    jmp saeos_task_entry_trampoline

global saeos_start_kernel_task
saeos_start_kernel_task:
    cli
    mov rsp, rsi
    and rsp, -16
    sub rsp, 8
    sti
    mov r12, rdi
    mov rdi, r12
    jmp saeos_task_entry_trampoline
