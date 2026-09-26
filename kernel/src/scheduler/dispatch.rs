use super::*;

/// Called from the assembly timer entry while interrupts are disabled. The
/// scheduler lock is released before the assembly path restores the returned
/// task's stack and executes iretq.
#[repr(C)]
pub struct TimerDispatch {
    pub context_pointer: u64,
    pub voluntary: u64,
}

#[unsafe(no_mangle)]
pub extern "sysv64" fn saeos_timer_interrupt_rust(stack_pointer: u64) -> TimerDispatch {
    crate::time::on_timer_interrupt();
    interrupt_controller::notify_end_of_interrupt(pic::PIC_1_OFFSET);
    if !crate::time::take_reschedule_request() {
        return TimerDispatch {
            context_pointer: stack_pointer,
            voluntary: 0,
        };
    }
    let (dispatch, address_space) = with_scheduler(|scheduler| {
        let previous = scheduler.current();
        if let Some(current) = previous {
            let stack = scheduler
                .task(current)
                .expect("current task disappeared")
                .kernel_stack;
            assert!(
                stack_pointer >= stack.base && stack_pointer.saturating_add(18 * 8) <= stack.top,
                "timer interrupt frame is outside the current task's kernel stack"
            );
        }
        scheduler.wake_expired(crate::time::ticks());
        let next = scheduler.preempt(stack_pointer, 1);
        let task = scheduler.task(next).expect("selected task disappeared");
        let address_space = task.address_space;
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
                && dispatch.context_pointer.saturating_add(18 * 8) <= selected_stack.top
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
        (dispatch, address_space)
    });
    crate::mm::address_space::activate_if_changed(address_space)
        .expect("selected address space is not registered");
    dispatch
}

pub(super) enum Dispatch {
    Voluntary(
        *mut VoluntaryContext,
        *const VoluntaryContext,
        Option<crate::task::AddressSpaceId>,
    ),
    Interrupt(
        *mut VoluntaryContext,
        u64,
        Option<crate::task::AddressSpaceId>,
    ),
}

pub(super) fn dispatch_for(scheduler: &mut Scheduler, current: TaskId, next: TaskId) -> Dispatch {
    let current_context = &mut scheduler
        .task_mut(current)
        .expect("current task disappeared")
        .cpu_context as *mut VoluntaryContext;
    let decision = scheduler
        .dispatch_decision(next)
        .expect("next task disappeared");
    match decision {
        DispatchDecision {
            resume_context: ResumeContext::Voluntary(_),
            address_space,
            ..
        } => {
            let next_task = scheduler.task(next).expect("next task disappeared");
            Dispatch::Voluntary(
                current_context,
                &next_task.cpu_context as *const VoluntaryContext,
                address_space,
            )
        }
        DispatchDecision {
            resume_context: ResumeContext::Interrupt { stack_pointer },
            address_space,
            ..
        } => {
            scheduler
                .task_mut(next)
                .expect("next task disappeared")
                .resume_kind = ResumeKind::Voluntary;
            Dispatch::Interrupt(current_context, stack_pointer, address_space)
        }
    }
}

pub(super) fn switch_dispatch(dispatch: Dispatch, interrupts_were_enabled: bool) {
    assert_switch_boundary_unlocked();
    match dispatch {
        Dispatch::Voluntary(current, next, address_space) => unsafe {
            crate::mm::address_space::activate_if_changed(address_space)
                .expect("selected address space is not registered");
            context::switch_context(&mut *current, &*next);
            if interrupts_were_enabled {
                cpu::sti();
            }
        },
        Dispatch::Interrupt(current, stack_pointer, address_space) => unsafe {
            crate::mm::address_space::activate_if_changed(address_space)
                .expect("selected address space is not registered");
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
