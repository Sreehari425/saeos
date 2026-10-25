use super::*;

/// Called from the assembly timer entry while interrupts are disabled. The
/// scheduler lock is released before the assembly path restores the returned
/// task's stack and executes iretq.
#[repr(C)]
pub struct TimerDispatch {
    pub context_pointer: u64,
    pub voluntary: u64,
}

/// Saved interrupt frame: 15 general-purpose registers pushed by the entry
/// stub plus RIP, CS, RFLAGS, RSP, SS (always pushed in long mode).
const INTERRUPT_FRAME_BYTES: u64 = 20 * 8;

/// Timer interrupts that arrived while user code was running.
static USER_PREEMPTIONS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn user_preemptions() -> u64 {
    USER_PREEMPTIONS.load(core::sync::atomic::Ordering::Relaxed)
}

#[unsafe(no_mangle)]
pub extern "sysv64" fn saeos_timer_interrupt_rust(stack_pointer: u64) -> TimerDispatch {
    let _irq = crate::arch::x86_64::irq::enter();
    // The saved CS sits after the 15 registers and RIP.
    let code_segment = unsafe { *((stack_pointer + 16 * 8) as *const u64) };
    if crate::arch::x86_64::syscall::from_user(code_segment) {
        USER_PREEMPTIONS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }
    crate::time::on_timer_interrupt();
    interrupt_controller::notify_end_of_interrupt(pic::PIC_1_OFFSET);
    let now = crate::time::now_ns();
    let ticks = crate::time::ticks();
    let selected = with_scheduler(|scheduler| {
        scheduler.wake_expired(ticks);
        // Always consume an explicit request, even when the tick alone
        // already asks for a switch.
        let requested = crate::time::take_reschedule_request();
        if !(scheduler.tick(now) | requested) {
            return None;
        }
        let previous = scheduler.current();
        if let Some(current) = previous {
            let stack = scheduler
                .task(current)
                .expect("current task disappeared")
                .kernel_stack;
            assert!(
                stack_pointer >= stack.base
                    && stack_pointer.saturating_add(INTERRUPT_FRAME_BYTES) <= stack.top,
                "timer interrupt frame is outside the current task's kernel stack"
            );
        }
        let outgoing_fpu = previous.map_or(core::ptr::null_mut(), |current| {
            &mut scheduler
                .task_mut(current)
                .expect("current task disappeared")
                .fpu as *mut FpuState
        });
        let next = scheduler.preempt(stack_pointer, now);
        let task = scheduler.task(next).expect("selected task disappeared");
        let target = task.switch_target();
        let resume_kind = task.resume_kind;
        let interrupt_stack_pointer = task.interrupt_stack_pointer;
        let selected_stack = task.kernel_stack;
        let voluntary_context = &task.cpu_context as *const VoluntaryContext as u64;
        let dispatch = match resume_kind {
            ResumeKind::Interrupt => TimerDispatch {
                context_pointer: interrupt_stack_pointer,
                voluntary: 0,
            },
            ResumeKind::Voluntary => TimerDispatch {
                context_pointer: voluntary_context,
                voluntary: 1,
            },
        };
        let selected_context_valid = if dispatch.voluntary != 0 {
            task.cpu_context.stack_pointer >= selected_stack.base
                && task.cpu_context.stack_pointer <= selected_stack.top
        } else {
            dispatch.context_pointer >= selected_stack.base
                && dispatch
                    .context_pointer
                    .saturating_add(INTERRUPT_FRAME_BYTES)
                    <= selected_stack.top
        };
        assert!(
            selected_context_valid,
            "selected task resume context is outside its kernel stack"
        );
        // The saved interrupt frame is one-shot. Once selected, it will be
        // consumed by iretq; a later scheduling point must save a fresh frame
        // or voluntary continuation before this task can be selected again.
        if resume_kind == ResumeKind::Interrupt {
            scheduler
                .task_mut(next)
                .expect("selected task disappeared")
                .resume_kind = ResumeKind::Voluntary;
        }
        Some((dispatch, outgoing_fpu, target))
    });
    let Some((dispatch, outgoing_fpu, target)) = selected else {
        // Keep running the interrupted task.
        return TimerDispatch {
            context_pointer: stack_pointer,
            voluntary: 0,
        };
    };
    unsafe { prepare_switch(outgoing_fpu, &target) };
    dispatch
}

/// Load everything the incoming task needs besides its registers. Every
/// switch goes through here: the timer path, voluntary switches, exit, and
/// the first task. `outgoing_fpu` is null when the outgoing task never runs
/// again (exit, boot).
///
/// # Safety
/// The pointers in `target` and `outgoing_fpu` must refer to live TCBs, and
/// the context switch must follow without any FPU use in between.
pub(super) unsafe fn prepare_switch(outgoing_fpu: *mut FpuState, target: &SwitchTarget) {
    crate::mm::address_space::activate_if_changed(target.address_space)
        .expect("selected address space is not registered");
    crate::arch::x86_64::gdt::set_kernel_stack(target.kernel_stack_top);
    unsafe { crate::arch::x86_64::fpu::switch(outgoing_fpu, target.fpu) };
}

pub(super) enum Dispatch {
    Voluntary(
        *mut VoluntaryContext,
        *const VoluntaryContext,
        *mut FpuState,
        SwitchTarget,
    ),
    Interrupt(*mut VoluntaryContext, u64, *mut FpuState, SwitchTarget),
}

pub(super) fn dispatch_for(scheduler: &mut Scheduler, current: TaskId, next: TaskId) -> Dispatch {
    let current_task = scheduler
        .task_mut(current)
        .expect("current task disappeared");
    let current_context = &mut current_task.cpu_context as *mut VoluntaryContext;
    let outgoing_fpu = &mut current_task.fpu as *mut FpuState;
    let target = scheduler
        .task(next)
        .expect("next task disappeared")
        .switch_target();
    let decision = scheduler
        .dispatch_decision(next)
        .expect("next task disappeared");
    match decision {
        DispatchDecision {
            resume_context: ResumeContext::Voluntary(_),
            ..
        } => {
            let next_task = scheduler.task(next).expect("next task disappeared");
            Dispatch::Voluntary(
                current_context,
                &next_task.cpu_context as *const VoluntaryContext,
                outgoing_fpu,
                target,
            )
        }
        DispatchDecision {
            resume_context: ResumeContext::Interrupt { stack_pointer },
            ..
        } => {
            scheduler
                .task_mut(next)
                .expect("next task disappeared")
                .resume_kind = ResumeKind::Voluntary;
            Dispatch::Interrupt(current_context, stack_pointer, outgoing_fpu, target)
        }
    }
}

pub(super) fn switch_dispatch(dispatch: Dispatch, interrupts_were_enabled: bool) {
    assert_switch_boundary_unlocked();
    match dispatch {
        Dispatch::Voluntary(current, next, outgoing_fpu, target) => unsafe {
            prepare_switch(outgoing_fpu, &target);
            context::switch_context(&mut *current, &*next);
            if interrupts_were_enabled {
                cpu::sti();
            }
        },
        Dispatch::Interrupt(current, stack_pointer, outgoing_fpu, target) => unsafe {
            prepare_switch(outgoing_fpu, &target);
            context::switch_to_interrupt_context(&mut *current, stack_pointer);
            if interrupts_were_enabled {
                cpu::sti();
            }
        },
    }
}

pub(super) fn assert_switch_boundary_unlocked() {
    assert!(
        !SCHEDULER.is_locked() && !PROCESSES.is_locked(),
        "scheduler/process lock held at switch or halt boundary"
    );
}
