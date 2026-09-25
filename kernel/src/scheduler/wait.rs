use super::*;

pub fn yield_now() {
    let interrupts_were_enabled = cpu::interrupts_enabled();
    cpu::cli();
    let dispatch = with_scheduler(|scheduler| {
        let (current, next) = scheduler
            .prepare_voluntary_yield()
            .expect("yield without task");
        dispatch_for(scheduler, current, next)
    });
    switch_dispatch(dispatch, interrupts_were_enabled)
}

pub fn sleep_until(deadline: u64) {
    let interrupts_were_enabled = cpu::interrupts_enabled();
    cpu::cli();
    let dispatch = with_scheduler(|scheduler| {
        let current = scheduler.current().expect("sleep without task");
        scheduler
            .sleep_current_until(deadline)
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

pub fn block_current(reason: BlockReason, queue: &mut WaitQueue) {
    let interrupts_were_enabled = cpu::interrupts_enabled();
    cpu::cli();
    let dispatch = with_scheduler(|scheduler| {
        let current = scheduler.current().expect("block without task");
        scheduler
            .block_current(reason, queue)
            .expect("invalid block transition");
        let next = scheduler.current().expect("block has no idle task");
        dispatch_for(scheduler, current, next)
    });
    record_waitqueue(queue);
    switch_dispatch(dispatch, interrupts_were_enabled)
}

pub fn block_on(reason: BlockReason, queue: &mut WaitQueue) {
    block_current(reason, queue)
}

pub fn wake_one(queue: &mut WaitQueue) -> Option<TaskId> {
    let woken = with_scheduler(|scheduler| scheduler.wake_one(queue).ok().flatten());
    record_waitqueue(queue);
    if woken.is_some() {
        crate::time::request_reschedule();
    }
    woken
}

pub fn wake_all(queue: &mut WaitQueue) -> usize {
    let woken = with_scheduler(|scheduler| scheduler.wake_all(queue).unwrap_or(0));
    record_waitqueue(queue);
    if woken > 0 {
        crate::time::request_reschedule();
    }
    woken
}

pub fn exit(status: ExitStatus) -> ! {
    exit_current(status)
}
