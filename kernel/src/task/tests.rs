use super::*;

fn stack() -> KernelStack {
    KernelStack {
        base: 0x1000,
        top: 0x2000,
    }
}

#[test]
fn state_cannot_pair_ready_with_block_reason() {
    assert!(matches!(TaskState::Ready, TaskState::Ready));
    assert_eq!(TaskState::Blocked, TaskState::Blocked);
}

#[test]
fn tasks_receive_cpu_time_and_exit_to_reaper() {
    let mut scheduler = Scheduler::new();
    let first = scheduler.spawn(stack(), "first");
    let second = scheduler.spawn(stack(), "second");
    assert_eq!(scheduler.schedule(0).unwrap(), Some(first));
    assert_eq!(scheduler.schedule(1).unwrap(), Some(second));
    scheduler.exit_current(ExitStatus(7)).unwrap();
    assert_eq!(scheduler.task(second).unwrap().state, TaskState::Exited);
    assert_eq!(
        scheduler.task(second).unwrap().exit_status,
        Some(ExitStatus(7))
    );
    assert_eq!(scheduler.reap_one().unwrap().unwrap().task_id, second);
}

#[test]
fn blocked_task_is_woken_fifo() {
    let mut scheduler = Scheduler::new();
    let first = scheduler.spawn(stack(), "first");
    let second = scheduler.spawn(stack(), "second");
    let mut queue = WaitQueue::new(1);
    assert_eq!(scheduler.schedule(0).unwrap(), Some(first));
    scheduler
        .block_current(BlockReason::WaitQueue(1), &mut queue)
        .unwrap();
    assert_eq!(scheduler.schedule(0).unwrap(), Some(second));
    scheduler
        .block_current(BlockReason::WaitQueue(1), &mut queue)
        .unwrap();
    assert_eq!(queue.len(), 2);
    assert_eq!(scheduler.wake_one(&mut queue).unwrap(), Some(first));
    assert_eq!(scheduler.task(first).unwrap().wait_link, None);
    assert_eq!(scheduler.wake_one(&mut queue).unwrap(), Some(second));
}

#[test]
fn wait_queue_supports_safe_arbitrary_removal() {
    let mut queue = WaitQueue::new(9);
    queue.tasks.extend([TaskId(1), TaskId(2), TaskId(3)]);
    assert!(queue.remove(TaskId(2)));
    assert!(!queue.remove(TaskId(2)));
    assert_eq!(queue.tasks, [TaskId(1), TaskId(3)]);
}

#[test]
fn cancel_wait_unlinks_and_makes_task_runnable() {
    let mut scheduler = Scheduler::new();
    let task = scheduler.spawn(stack(), "waiter");
    let mut queue = WaitQueue::new(9);
    scheduler.schedule(0).unwrap();
    scheduler
        .block_current(BlockReason::WaitQueue(9), &mut queue)
        .unwrap();
    scheduler.cancel_wait(task, &mut queue).unwrap();
    assert_eq!(queue.len(), 0);
    assert_eq!(scheduler.task(task).unwrap().state, TaskState::Ready);
    assert_eq!(scheduler.task(task).unwrap().wait_link, None);
}

#[test]
fn exited_task_cannot_transition_back_to_ready() {
    assert!(!TaskState::Exited.can_transition(TaskState::Ready));
    assert!(!TaskState::Blocked.can_transition(TaskState::Running));
    assert!(TaskState::Blocked.can_transition(TaskState::Ready));
}

#[test]
fn scheduling_entity_accounts_weighted_runtime_and_enqueue_time() {
    let mut entity = EevdfEntity::default();
    entity.enqueue(11, 2, 47);
    assert_eq!(entity.virtual_runtime, 11);
    assert_eq!(entity.enqueue_time, 47);
    assert_eq!(entity.requested_slice, TARGET_LATENCY / 2);
    entity.account(2);
    assert_eq!(entity.exec_runtime, 2);
    assert_eq!(entity.virtual_runtime, 13);
}

#[test]
fn eevdf_prefers_earliest_eligible_deadline() {
    let mut scheduler = Scheduler::new();
    let first = scheduler.spawn(stack(), "first");
    let second = scheduler.spawn(stack(), "second");
    scheduler
        .task_mut(first)
        .unwrap()
        .scheduler_entity
        .virtual_deadline = 20;
    scheduler
        .task_mut(second)
        .unwrap()
        .scheduler_entity
        .virtual_deadline = 10;
    assert_eq!(scheduler.schedule(0).unwrap(), Some(second));
}

#[test]
fn scheduler_policy_can_be_selected() {
    let mut scheduler = Scheduler::with_policy(PolicyKind::Fifo);
    let first = scheduler.spawn(stack(), "first");
    let second = scheduler.spawn(stack(), "second");
    assert_eq!(scheduler.schedule(0).unwrap(), Some(first));
    assert_eq!(scheduler.schedule(1).unwrap(), Some(second));
}

#[test]
fn fifo_holds_cpu_across_timer_tick_but_round_robin_switches() {
    let mut fifo = Scheduler::with_policy(PolicyKind::Fifo);
    let fifo_first = fifo.spawn(stack(), "first");
    fifo.spawn(stack(), "second");
    fifo.schedule(0).unwrap();
    assert_eq!(fifo.preempt(0x1000, 1), fifo_first);

    let mut rr = Scheduler::with_policy(PolicyKind::RoundRobin);
    let rr_first = rr.spawn(stack(), "first");
    let rr_second = rr.spawn(stack(), "second");
    rr.schedule(0).unwrap();
    assert_eq!(rr.preempt(0x1000, 1), rr_second);
    assert_ne!(rr_first, rr_second);
}

#[test]
fn waking_a_registered_child_waiter_transitions_blocked_task() {
    let mut scheduler = Scheduler::new();
    let waiter = scheduler.spawn(stack(), "parent");
    scheduler.spawn(stack(), "other");
    assert_eq!(scheduler.schedule(0).unwrap(), Some(waiter));
    let mut queue = WaitQueue::new(77);
    scheduler
        .block_current(BlockReason::WaitingForChild(ProcessId(9)), &mut queue)
        .unwrap();
    scheduler.wake_task(waiter).unwrap();
    assert_eq!(scheduler.task(waiter).unwrap().state, TaskState::Ready);
}

#[test]
fn sleeping_task_is_not_runnable_until_deadline() {
    let mut scheduler = Scheduler::new();
    let first = scheduler.spawn(stack(), "first");
    let second = scheduler.spawn(stack(), "second");
    assert_eq!(scheduler.schedule(0).unwrap(), Some(first));
    scheduler.sleep_current_until(10).unwrap();
    assert_eq!(scheduler.current(), Some(second));
    assert_eq!(scheduler.wake_expired(9), 0);
    assert_eq!(scheduler.wake_expired(10), 1);
    assert!(matches!(
        scheduler.task(first).unwrap().state,
        TaskState::Ready
    ));
}

#[test]
fn dispatch_decision_preserves_context_kind() {
    let mut scheduler = Scheduler::new();
    let first = scheduler.spawn(stack(), "first");
    let second = scheduler.spawn(stack(), "second");
    scheduler.task_mut(first).unwrap().resume_kind = ResumeKind::Voluntary;
    scheduler.task_mut(first).unwrap().cpu_context = VoluntaryContext {
        stack_pointer: 0x1000,
        instruction_pointer: 0x2000,
        ..VoluntaryContext::ZERO
    };
    scheduler.task_mut(second).unwrap().resume_kind = ResumeKind::Interrupt;
    scheduler.task_mut(second).unwrap().interrupt_stack_pointer = 0x3000;

    assert!(matches!(
        scheduler.dispatch_decision(first).unwrap().resume_context,
        ResumeContext::Voluntary(VoluntaryContext {
            stack_pointer: 0x1000,
            instruction_pointer: 0x2000,
            ..
        })
    ));
    assert_eq!(
        scheduler.dispatch_decision(second).unwrap().resume_context,
        ResumeContext::Interrupt {
            stack_pointer: 0x3000
        }
    );
}

#[test]
fn voluntary_to_voluntary_dispatch() {
    let mut scheduler = Scheduler::new();
    let first = scheduler.spawn(stack(), "first");
    let second = scheduler.spawn(stack(), "second");
    scheduler
        .task_mut(first)
        .unwrap()
        .scheduler_entity
        .virtual_runtime = 100;
    scheduler.schedule(0).unwrap();
    let (_, next) = scheduler.prepare_voluntary_yield().unwrap();
    assert_eq!(next, second);
    assert!(matches!(
        scheduler.dispatch_decision(next).unwrap().resume_context,
        ResumeContext::Voluntary(_)
    ));
}

#[test]
fn voluntary_to_interrupt_dispatch() {
    let mut scheduler = Scheduler::new();
    let first = scheduler.spawn(stack(), "first");
    let second = scheduler.spawn(stack(), "second");
    scheduler.task_mut(second).unwrap().resume_kind = ResumeKind::Interrupt;
    scheduler.task_mut(second).unwrap().interrupt_stack_pointer = 0x3000;
    scheduler.schedule(0).unwrap();
    scheduler
        .task_mut(first)
        .unwrap()
        .scheduler_entity
        .virtual_runtime = 100;
    let (_, next) = scheduler.prepare_voluntary_yield().unwrap();
    assert_eq!(next, second);
    assert_eq!(
        scheduler.dispatch_decision(next).unwrap().resume_context,
        ResumeContext::Interrupt {
            stack_pointer: 0x3000
        }
    );
}

#[test]
fn interrupt_to_voluntary_dispatch() {
    let mut scheduler = Scheduler::new();
    let first = scheduler.spawn(stack(), "first");
    let second = scheduler.spawn(stack(), "second");
    scheduler.task_mut(second).unwrap().resume_kind = ResumeKind::Voluntary;
    scheduler.schedule(0).unwrap();
    let next = scheduler.preempt(0x2800, 100);
    assert_eq!(next, second);
    assert!(matches!(
        scheduler.dispatch_decision(next).unwrap().resume_context,
        ResumeContext::Voluntary(_)
    ));
    assert_eq!(
        scheduler.dispatch_decision(first).unwrap().resume_context,
        ResumeContext::Interrupt {
            stack_pointer: 0x2800
        }
    );
}

#[test]
fn interrupt_to_interrupt_dispatch() {
    let mut scheduler = Scheduler::new();
    let first = scheduler.spawn(stack(), "first");
    let second = scheduler.spawn(stack(), "second");
    scheduler.task_mut(first).unwrap().resume_kind = ResumeKind::Interrupt;
    scheduler.task_mut(second).unwrap().resume_kind = ResumeKind::Interrupt;
    scheduler.task_mut(second).unwrap().interrupt_stack_pointer = 0x3800;
    scheduler.schedule(0).unwrap();
    let next = scheduler.preempt(0x2800, 100);
    assert_eq!(next, second);
    assert_eq!(
        scheduler.dispatch_decision(next).unwrap().resume_context,
        ResumeContext::Interrupt {
            stack_pointer: 0x3800
        }
    );
}

#[test]
fn dispatch_keeps_address_space_identity() {
    let mut scheduler = Scheduler::new();
    let task = scheduler.spawn(stack(), "task");
    scheduler
        .set_address_space(task, Some(AddressSpaceId(9)))
        .unwrap();
    assert_eq!(
        scheduler.dispatch_decision(task).unwrap().address_space,
        Some(AddressSpaceId(9))
    );
}
