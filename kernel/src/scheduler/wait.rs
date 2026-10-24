use super::*;

pub fn yield_now() {
    let interrupts_were_enabled = cpu::interrupts_enabled();
    cpu::cli();
    let now = crate::time::now_ns();
    let dispatch = with_scheduler(|scheduler| {
        let (current, next) = scheduler
            .prepare_voluntary_yield(now)
            .expect("yield without task");
        dispatch_for(scheduler, current, next)
    });
    switch_dispatch(dispatch, interrupts_were_enabled)
}

pub fn sleep_until(deadline: u64) {
    let interrupts_were_enabled = cpu::interrupts_enabled();
    cpu::cli();
    let now = crate::time::now_ns();
    let dispatch = with_scheduler(|scheduler| {
        let current = scheduler.current().expect("sleep without task");
        scheduler
            .sleep_current_until(deadline, now)
            .expect("invalid sleep transition");
        let next = scheduler.current().expect("sleep has no idle task");
        dispatch_for(scheduler, current, next)
    });
    scheduler_debug(format_args!("task blocked until tick {}", deadline));
    switch_dispatch(dispatch, interrupts_were_enabled)
}

pub fn sleep_ms(milliseconds: u64) {
    sleep_until(crate::time::ticks().saturating_add(milliseconds))
}

/// Sleep on `queue` until `condition` holds.
///
/// The condition is checked with interrupts disabled, and they stay
/// disabled through enqueueing on `queue` and switching away. A wake-up from
/// an interrupt handler therefore cannot slip in between the check and the
/// sleep (the single-CPU form of Linux's `prepare_to_wait`). The condition
/// is re-checked after every wake-up, so spurious or shared wake-ups are
/// harmless.
///
/// Lock order: `queue`, then the scheduler. `condition` runs with neither
/// held and must not block.
pub fn wait_event(queue: &'static IrqSpinMutex<WaitQueue>, condition: impl Fn() -> bool) {
    loop {
        let interrupts_were_enabled = cpu::interrupts_enabled();
        cpu::cli();
        if condition() {
            if interrupts_were_enabled {
                cpu::sti();
            }
            return;
        }
        let now = crate::time::now_ns();
        let dispatch = {
            let mut queue = queue.lock();
            let dispatch = with_scheduler(|scheduler| {
                let current = scheduler.current().expect("wait without task");
                let reason = BlockReason::WaitQueue(queue.id);
                scheduler
                    .block_current(reason, &mut queue, now)
                    .expect("invalid block transition");
                let next = scheduler.current().expect("wait has no idle task");
                dispatch_for(scheduler, current, next)
            });
            record_waitqueue(&queue, true);
            dispatch
        };
        // Both locks are released; interrupts stay off until the switch.
        switch_dispatch(dispatch, interrupts_were_enabled);
    }
}

/// Wake the longest-waiting task on `queue`. Safe from interrupt handlers.
pub fn wake_one(queue: &'static IrqSpinMutex<WaitQueue>) -> Option<TaskId> {
    let mut queue = queue.lock();
    if queue.is_empty() {
        return None;
    }
    let woken = with_scheduler(|scheduler| scheduler.wake_one(&mut queue).ok().flatten());
    record_waitqueue(&queue, false);
    woken
}

/// Wake every task on `queue`. Safe from interrupt handlers.
pub fn wake_all(queue: &'static IrqSpinMutex<WaitQueue>) -> usize {
    let mut queue = queue.lock();
    if queue.is_empty() {
        return 0;
    }
    let woken = with_scheduler(|scheduler| scheduler.wake_all(&mut queue).unwrap_or(0));
    record_waitqueue(&queue, false);
    woken
}

pub fn exit(status: ExitStatus) -> ! {
    exit_current(status)
}
