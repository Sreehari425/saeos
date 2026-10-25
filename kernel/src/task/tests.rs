use super::*;

fn stack() -> KernelStack {
    KernelStack {
        base: 0x1000,
        top: 0x2000,
    }
}

const MS: Nanos = 1_000_000;

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
    assert_eq!(scheduler.schedule(MS).unwrap(), Some(second));
    assert!(!scheduler.has_exited_tasks());
    scheduler.exit_current(ExitStatus(7), 2 * MS).unwrap();
    assert_eq!(scheduler.task(second).unwrap().state, TaskState::Exited);
    assert_eq!(
        scheduler.task(second).unwrap().exit_status,
        Some(ExitStatus(7))
    );
    assert!(scheduler.has_exited_tasks());
    assert_eq!(scheduler.reap_one().unwrap().unwrap().task_id, second);
    assert!(scheduler.task(second).is_none());
}

#[test]
fn blocked_task_is_woken_fifo() {
    let mut scheduler = Scheduler::new();
    let first = scheduler.spawn(stack(), "first");
    let second = scheduler.spawn(stack(), "second");
    let mut queue = WaitQueue::new(1);
    assert_eq!(scheduler.schedule(0).unwrap(), Some(first));
    scheduler
        .block_current(BlockReason::WaitQueue(1), &mut queue, MS)
        .unwrap();
    assert_eq!(scheduler.current(), Some(second));
    scheduler
        .block_current(BlockReason::WaitQueue(1), &mut queue, 2 * MS)
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
        .block_current(BlockReason::WaitQueue(9), &mut queue, MS)
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
fn entity_charges_weighted_virtual_time_and_reissues_deadlines() {
    let mut entity = EevdfEntity::default();
    entity.virtual_deadline = entity.weighted_slice();
    assert!(!entity.account(MS));
    assert_eq!(entity.exec_runtime, MS);
    assert_eq!(entity.virtual_runtime, MS as VirtualTime);
    // Finishing the 3 ms request issues the next deadline.
    assert!(entity.account(2 * MS));
    assert_eq!(
        entity.virtual_deadline,
        entity.virtual_runtime + BASE_SLICE_NS as VirtualTime
    );
    // Half the weight advances virtual time twice as fast.
    let mut light = EevdfEntity {
        weight: NICE_0_WEIGHT / 2,
        ..EevdfEntity::default()
    };
    light.virtual_deadline = VirtualTime::MAX;
    light.account(MS);
    assert_eq!(light.virtual_runtime, 2 * MS as VirtualTime);
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
fn ineligible_task_waits_even_with_earlier_deadline() {
    let mut scheduler = Scheduler::new();
    let ahead = scheduler.spawn(stack(), "ahead");
    let behind = scheduler.spawn(stack(), "behind");
    scheduler.set_virtual_runtime(ahead, 100 * MS as VirtualTime);
    scheduler
        .task_mut(ahead)
        .unwrap()
        .scheduler_entity
        .virtual_deadline = 0;
    assert_eq!(scheduler.schedule(0).unwrap(), Some(behind));
}

#[test]
fn scheduler_policy_can_be_selected() {
    let mut scheduler = Scheduler::with_policy(PolicyKind::Fifo);
    let first = scheduler.spawn(stack(), "first");
    let second = scheduler.spawn(stack(), "second");
    assert_eq!(scheduler.schedule(0).unwrap(), Some(first));
    assert_eq!(scheduler.schedule(MS).unwrap(), Some(second));
}

#[test]
fn fifo_never_preempts_but_round_robin_rotates_after_a_slice() {
    let mut fifo = Scheduler::with_policy(PolicyKind::Fifo);
    fifo.spawn(stack(), "first");
    fifo.spawn(stack(), "second");
    fifo.schedule(0).unwrap();
    assert!((1..100).all(|tick| !fifo.tick(tick * MS)));

    let mut rr = Scheduler::with_policy(PolicyKind::RoundRobin);
    let rr_first = rr.spawn(stack(), "first");
    let rr_second = rr.spawn(stack(), "second");
    assert_eq!(rr.schedule(0).unwrap(), Some(rr_first));
    assert!(!rr.tick(MS));
    assert!(!rr.tick(2 * MS));
    assert!(rr.tick(BASE_SLICE_NS));
    assert_eq!(rr.preempt(0x1000, BASE_SLICE_NS), rr_second);
}

#[test]
fn eevdf_preempts_only_when_the_request_completes() {
    let mut scheduler = Scheduler::new();
    let first = scheduler.spawn(stack(), "first");
    scheduler.spawn(stack(), "second");
    assert_eq!(scheduler.schedule(0).unwrap(), Some(first));
    // 1 ms ticks: no preemption until the 3 ms request is done.
    assert!(!scheduler.tick(MS));
    assert!(!scheduler.tick(2 * MS));
    assert!(scheduler.tick(3 * MS));
}

#[test]
fn lone_task_is_never_preempted_and_idle_yields_to_work() {
    let mut scheduler = Scheduler::new();
    let only = scheduler.spawn(stack(), "only");
    assert_eq!(scheduler.schedule(0).unwrap(), Some(only));
    assert!((1..50).all(|tick| !scheduler.tick(tick * MS)));
    scheduler.sleep_current_until(60, 50 * MS).unwrap();
    assert_eq!(scheduler.current(), None, "no idle task registered");
    assert!(!scheduler.tick(55 * MS));
    scheduler.wake_expired(60);
    assert!(
        scheduler.tick(60 * MS),
        "queued work must displace the idle path"
    );
}

#[test]
fn context_switch_is_not_counted_when_the_same_task_continues() {
    let mut scheduler = Scheduler::new();
    let only = scheduler.spawn(stack(), "only");
    scheduler.schedule(0).unwrap();
    let before = scheduler.stats().context_switches;
    assert_eq!(scheduler.preempt(0x1000, 10 * MS), only);
    assert_eq!(scheduler.stats().context_switches, before);
    assert_eq!(scheduler.stats().timer_preemptions, 0);
}

#[test]
fn yielding_early_still_pays_for_cpu_time() {
    let mut scheduler = Scheduler::new();
    let sneaky = scheduler.spawn(stack(), "sneaky");
    let honest = scheduler.spawn(stack(), "honest");
    let mut now = 0;
    assert_eq!(scheduler.schedule(now).unwrap(), Some(sneaky));
    // `sneaky` always yields 0.3 ms into its turn; `honest` runs its slices.
    for _ in 0..200 {
        let current = scheduler.current().unwrap();
        now += if current == sneaky {
            MS * 3 / 10
        } else {
            BASE_SLICE_NS
        };
        scheduler.prepare_voluntary_yield(now).unwrap();
    }
    let sneaky_runtime = scheduler
        .task(sneaky)
        .unwrap()
        .scheduler_entity
        .exec_runtime;
    let honest_runtime = scheduler
        .task(honest)
        .unwrap()
        .scheduler_entity
        .exec_runtime;
    assert_eq!(
        sneaky_runtime + honest_runtime,
        now,
        "all elapsed time is charged"
    );
    assert!(sneaky_runtime > 0);
    // Short turns mean more turns, but never more than a fair share.
    assert!(sneaky_runtime <= honest_runtime + BASE_SLICE_NS);
}

#[test]
fn waking_task_with_earlier_deadline_requests_preemption() {
    let mut scheduler = Scheduler::new();
    let sleeper = scheduler.spawn(stack(), "sleeper");
    let runner = scheduler.spawn(stack(), "runner");
    // The sleeper is owed CPU time (positive lag) when it goes to sleep, so
    // on waking it is eligible with an earlier deadline than the runner.
    scheduler.set_virtual_runtime(sleeper, -12 * MS as VirtualTime);
    assert_eq!(scheduler.schedule(0).unwrap(), Some(sleeper));
    scheduler.sleep_current_until(5, MS).unwrap();
    assert!(scheduler.task(sleeper).unwrap().scheduler_entity.lag > 0);
    assert_eq!(scheduler.current(), Some(runner));
    // The runner burns through requests while the sleeper is away.
    for tick in 2..=20 {
        scheduler.tick(tick * MS);
    }
    assert!(!scheduler.wakeup_preempt_pending());
    scheduler.wake_expired(20);
    assert!(scheduler.wakeup_preempt_pending());
    assert!(scheduler.tick(21 * MS));
    assert_eq!(scheduler.preempt(0x1000, 21 * MS), sleeper);
}

#[test]
fn lag_survives_a_sleep_within_the_clamp() {
    let mut scheduler = Scheduler::new();
    let owed = scheduler.spawn(stack(), "owed");
    let other = scheduler.spawn(stack(), "other");
    scheduler.set_virtual_runtime(owed, -(MS as VirtualTime));
    scheduler.set_virtual_runtime(other, MS as VirtualTime);
    assert_eq!(scheduler.schedule(0).unwrap(), Some(owed));
    scheduler.sleep_current_until(10, 0).unwrap();
    let saved = scheduler.task(owed).unwrap().scheduler_entity.lag;
    assert_eq!(saved, MS as VirtualTime, "owed 1 ms below V");
    scheduler.wake_expired(10);
    // After rejoining, the task sits exactly `saved` below the new V.
    let average = scheduler.average_vruntime();
    let runtime = scheduler
        .task(owed)
        .unwrap()
        .scheduler_entity
        .virtual_runtime;
    assert!(
        (average - runtime - saved).abs() <= 1,
        "lag {} != {saved}",
        average - runtime
    );

    // Extreme lag is clamped.
    let mut far = Scheduler::new();
    let hoarder = far.spawn(stack(), "hoarder");
    far.spawn(stack(), "other");
    far.set_virtual_runtime(hoarder, -1_000 * MS as VirtualTime);
    far.schedule(0).unwrap();
    far.sleep_current_until(10, 0).unwrap();
    assert_eq!(far.task(hoarder).unwrap().scheduler_entity.lag, LAG_LIMIT);
}

#[test]
fn cpu_share_follows_nice_weights() {
    let mut scheduler = Scheduler::new();
    let normal = scheduler.spawn(stack(), "nice0");
    let nicer = scheduler.spawn(stack(), "nice5");
    scheduler.set_nice(nicer, 5).unwrap();
    assert_eq!(scheduler.task(nicer).unwrap().scheduler_entity.weight, 335);
    let mut now = 0;
    scheduler.schedule(now).unwrap();
    // 10 simulated seconds of 1 ms ticks (1 s under Miri).
    let ticks = if cfg!(miri) { 1_000 } else { 10_000 };
    for _ in 0..ticks {
        now += MS;
        if scheduler.tick(now) {
            scheduler.preempt(0x1000, now);
        }
    }
    scheduler.schedule(now).unwrap();
    let a = scheduler
        .task(normal)
        .unwrap()
        .scheduler_entity
        .exec_runtime as f64;
    let b = scheduler.task(nicer).unwrap().scheduler_entity.exec_runtime as f64;
    let ratio = a / b;
    let expected = 1024.0 / 335.0;
    assert!(
        (ratio / expected - 1.0).abs() < if cfg!(miri) { 0.15 } else { 0.05 },
        "runtime ratio {ratio:.3}, expected {expected:.3}"
    );
}

#[test]
fn interrupt_paths_do_not_allocate() {
    let mut scheduler = Scheduler::new();
    let tasks: Vec<_> = (0..8).map(|_| scheduler.spawn(stack(), "worker")).collect();
    let mut queue = WaitQueue::new(3);
    scheduler.schedule(0).unwrap();
    // Park half the tasks on sleeps and the queue (task context: may allocate).
    for (index, _) in tasks.iter().enumerate().take(4) {
        let now = (index as Nanos + 1) * MS;
        if index % 2 == 0 {
            scheduler
                .sleep_current_until(20 + index as Tick, now)
                .unwrap();
        } else {
            scheduler
                .block_current(BlockReason::WaitQueue(3), &mut queue, now)
                .unwrap();
        }
    }
    let (_, allocations) = crate::alloc_counter::allocations_during(|| {
        let mut now = 10 * MS;
        for tick in 10..40 {
            now = tick * MS;
            scheduler.wake_expired(tick);
            if scheduler.tick(now) {
                scheduler.preempt(0x1000, now);
            }
        }
        while scheduler.wake_one(&mut queue).unwrap().is_some() {}
        scheduler.tick(now + MS);
    });
    assert_eq!(allocations, 0, "timer and wake paths must not allocate");
}

#[test]
fn task_storage_is_stable_across_spawn_and_reap() {
    let mut scheduler = Scheduler::new();
    let anchor = scheduler.spawn(stack(), "anchor");
    let address = scheduler.task(anchor).unwrap() as *const TaskControlBlock;
    let mut now = 0;
    scheduler.schedule(now).unwrap();
    for _ in 0..100 {
        let worker = scheduler.spawn(stack(), "worker");
        now += MS;
        while scheduler.current() != Some(worker) {
            scheduler.prepare_voluntary_yield(now).unwrap();
            now += MS;
        }
        scheduler.exit_current(ExitStatus(0), now).unwrap();
        while scheduler.reap_one().unwrap().is_some() {}
        if scheduler.current().is_none() {
            scheduler.schedule(now).unwrap();
        }
    }
    assert_eq!(
        scheduler.task(anchor).unwrap() as *const TaskControlBlock,
        address,
        "a live task's TCB must never move"
    );
    assert_eq!(scheduler.snapshots().len(), 1);
}

#[test]
fn waking_a_registered_child_waiter_transitions_blocked_task() {
    let mut scheduler = Scheduler::new();
    let waiter = scheduler.spawn(stack(), "parent");
    scheduler.spawn(stack(), "other");
    assert_eq!(scheduler.schedule(0).unwrap(), Some(waiter));
    let mut queue = WaitQueue::new(77);
    scheduler
        .block_current(BlockReason::WaitingForChild(ProcessId(9)), &mut queue, MS)
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
    scheduler.sleep_current_until(10, MS).unwrap();
    assert_eq!(scheduler.current(), Some(second));
    assert_eq!(scheduler.wake_expired(9), 0);
    assert_eq!(scheduler.wake_expired(10), 1);
    assert!(matches!(
        scheduler.task(first).unwrap().state,
        TaskState::Ready
    ));
}

#[test]
fn sleepers_wake_in_deadline_order_and_stale_entries_are_skipped() {
    let mut scheduler = Scheduler::new();
    let ids: Vec<_> = (0..4)
        .map(|_| scheduler.spawn(stack(), "sleeper"))
        .collect();
    scheduler.spawn(stack(), "runner");
    scheduler.schedule(0).unwrap();
    for deadline in [30, 10, 20, 40] {
        scheduler.sleep_current_until(deadline, 0).unwrap();
    }
    assert!(
        scheduler
            .sleepers()
            .windows(2)
            .all(|pair| pair[0] >= pair[1])
    );
    // Wake the 20-tick sleeper early; its entry must not wake it twice.
    let early = ids
        .iter()
        .copied()
        .find(|id| scheduler.task(*id).unwrap().block_reason == Some(BlockReason::SleepUntil(20)))
        .unwrap();
    scheduler.wake_task(early).unwrap();
    assert_eq!(scheduler.wake_expired(25), 1);
    assert_eq!(scheduler.wake_expired(40), 2);
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
    assert_eq!(scheduler.schedule(0).unwrap(), Some(first));
    // After 1 ms, `first` is ahead of V and no longer eligible.
    let (_, next) = scheduler.prepare_voluntary_yield(MS).unwrap();
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
    assert_eq!(scheduler.current(), Some(first));
    let (_, next) = scheduler.prepare_voluntary_yield(MS).unwrap();
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
    let next = scheduler.preempt(0x2800, BASE_SLICE_NS);
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
    let next = scheduler.preempt(0x2800, BASE_SLICE_NS);
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
