//! Single-CPU EEVDF scheduler core.
//!
//! Pure bookkeeping: callers pass the current monotonic time (`now`, in ns)
//! and the architecture layer performs the actual switches, so the whole
//! policy runs unchanged in the host tests (`tools/host-tests`).
//!
//! EEVDF in brief: every runnable task (queued or running) contributes to a
//! weighted average virtual time `V`. A task is *eligible* when it has not
//! run ahead of `V`; among eligible tasks the one with the earliest virtual
//! deadline runs. Each task requests `BASE_SLICE_NS` of CPU at a time; the
//! timer preempts when that request completes or when a waking task has an
//! earlier deadline, not on every tick. Lag (`V - vruntime`) is saved when a
//! task stops being runnable and restored when it returns, so sleeping
//! neither earns nor loses CPU share.
//!
//! Nothing reachable from the timer interrupt (`tick`, `preempt`,
//! `wake_expired`, `wake_one`) allocates: queue capacity is reserved when
//! tasks are created.

use super::*;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

pub struct Scheduler {
    /// Boxed so every task's context stays at one address for the task's
    /// whole life: dispatch keeps raw pointers to it across the lock release.
    tasks: Vec<Option<Box<TaskControlBlock>>>,
    free_slots: Vec<usize>,
    index: BTreeMap<TaskId, usize>,
    /// Ready tasks, excluding the running one and the idle task.
    run_queue: Vec<TaskId>,
    exit_queue: VecDeque<TaskId>,
    next_task_id: u64,
    current: Option<TaskId>,
    idle: Option<TaskId>,
    /// Sleeping tasks, sorted by descending `(deadline, id)` so the next to
    /// wake is at the end.
    sleepers: Vec<(Tick, TaskId)>,
    /// Sums over the runnable set (queued and running, never idle) for the
    /// weighted average `V`. `i128` virtual time cannot overflow in any
    /// realistic uptime, so no rebasing is needed.
    sum_weight: VirtualTime,
    sum_weighted_runtime: VirtualTime,
    /// Last computed `V`, kept when the runnable set is empty so new tasks
    /// join at a continuous virtual time.
    average: VirtualTime,
    /// A task woke with an earlier eligible deadline than the running one.
    wakeup_preempt: bool,
    clock: Nanos,
    policy: PolicyKind,
    stats: SchedulerStats,
}

/// Selects the run-queue ordering strategy. EEVDF is the production default;
/// FIFO and round-robin are available for scheduler comparisons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyKind {
    Eevdf,
    Fifo,
    RoundRobin,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scheduler {
    pub const fn new() -> Self {
        Self {
            tasks: Vec::new(),
            free_slots: Vec::new(),
            index: BTreeMap::new(),
            run_queue: Vec::new(),
            exit_queue: VecDeque::new(),
            next_task_id: 1,
            current: None,
            idle: None,
            sleepers: Vec::new(),
            sum_weight: 0,
            sum_weighted_runtime: 0,
            average: 0,
            wakeup_preempt: false,
            clock: 0,
            policy: PolicyKind::Eevdf,
            stats: SchedulerStats {
                context_switches: 0,
                timer_preemptions: 0,
                voluntary_yields: 0,
                tasks_woken: 0,
                tasks_reaped: 0,
                idle_ticks: 0,
            },
        }
    }

    pub fn with_policy(policy: PolicyKind) -> Self {
        let mut scheduler = Self::new();
        scheduler.policy = policy;
        scheduler
    }

    pub fn spawn(&mut self, kernel_stack: KernelStack, name: &'static str) -> TaskId {
        let id = self.allocate_id();
        let mut task = TaskControlBlock::new(id, kernel_stack, name);
        task.transition(TaskState::Ready)
            .expect("new task transition");
        self.insert_task(task);
        self.join(id);
        id
    }

    /// Create a kernel task that starts on `storage` through the voluntary
    /// bootstrap path. The caller allocates the stack before taking the
    /// scheduler lock, so the scheduler never calls into the memory manager.
    pub fn spawn_kernel_task(
        &mut self,
        entry: TaskEntry,
        name: &'static str,
        is_idle: bool,
        storage: StackStorage,
    ) -> TaskId {
        let id = self.allocate_id();
        let KernelStack { base, top } = stack_storage_bounds(&storage);
        #[cfg(not(host_model))]
        let bootstrap_ip = crate::arch::x86_64::context::task_bootstrap_ip();
        #[cfg(host_model)]
        let bootstrap_ip = 0;
        let mut task = TaskControlBlock::new(id, KernelStack { base, top }, name);
        task.transition(TaskState::Ready)
            .expect("new kernel task transition");
        task.cpu_context = VoluntaryContext {
            stack_pointer: top - 8,
            instruction_pointer: bootstrap_ip,
            r12: entry as usize as u64,
            ..VoluntaryContext::ZERO
        };
        task.resume_kind = ResumeKind::Voluntary;
        task.stack_storage = Some(storage);
        task.entry = Some(entry);
        task.is_idle = is_idle;
        self.insert_task(task);
        if is_idle {
            self.idle = Some(id);
        } else {
            self.join(id);
        }
        id
    }

    fn allocate_id(&mut self) -> TaskId {
        let id = TaskId(self.next_task_id);
        self.next_task_id = self.next_task_id.saturating_add(1);
        id
    }

    /// Store a new task. Runs in task context only, so this is where every
    /// queue that interrupt paths push into gets its capacity reserved.
    fn insert_task(&mut self, task: TaskControlBlock) {
        let id = task.task_id;
        let slot = self.free_slots.pop().unwrap_or_else(|| {
            self.tasks.push(None);
            self.tasks.len() - 1
        });
        self.tasks[slot] = Some(Box::new(task));
        self.index.insert(id, slot);
        let tasks = self.index.len();
        self.run_queue
            .reserve(tasks.saturating_sub(self.run_queue.len()));
    }

    pub fn current(&self) -> Option<TaskId> {
        self.current
    }

    /// The task whose kernel stack starts at `base`, without allocating.
    pub fn task_on_stack(&self, base: u64) -> Option<(TaskId, &'static str)> {
        self.tasks
            .iter()
            .flatten()
            .find(|task| task.kernel_stack.base == base)
            .map(|task| (task.task_id, task.name))
    }

    pub fn task(&self, id: TaskId) -> Option<&TaskControlBlock> {
        let slot = *self.index.get(&id)?;
        self.tasks[slot].as_deref()
    }

    pub fn task_mut(&mut self, id: TaskId) -> Option<&mut TaskControlBlock> {
        let slot = *self.index.get(&id)?;
        self.tasks[slot].as_deref_mut()
    }

    pub fn runnable_count(&self) -> usize {
        self.run_queue.len() + usize::from(self.current.is_some() && self.current != self.idle)
    }

    pub fn idle_task(&self) -> Option<TaskId> {
        self.idle
    }

    pub fn run_queue(&self) -> &[TaskId] {
        &self.run_queue
    }

    pub fn sleepers(&self) -> &[(Tick, TaskId)] {
        &self.sleepers
    }

    pub fn stats(&self) -> SchedulerStats {
        self.stats
    }

    pub fn has_exited_tasks(&self) -> bool {
        !self.exit_queue.is_empty()
    }

    /// The weighted average virtual time `V` of the runnable set.
    pub fn average_vruntime(&self) -> VirtualTime {
        self.average
    }

    pub fn dispatch_decision(&self, task_id: TaskId) -> Result<DispatchDecision, SchedulerError> {
        let task = self.task(task_id).ok_or(SchedulerError::UnknownTask)?;
        let resume_context = match task.resume_kind {
            ResumeKind::Voluntary => ResumeContext::Voluntary(task.cpu_context),
            ResumeKind::Interrupt => ResumeContext::Interrupt {
                stack_pointer: task.interrupt_stack_pointer,
            },
        };
        Ok(DispatchDecision {
            task_id,
            resume_context,
            address_space: task.address_space,
        })
    }

    pub fn set_address_space(
        &mut self,
        task_id: TaskId,
        address_space: Option<AddressSpaceId>,
    ) -> Result<(), SchedulerError> {
        let task = self.task_mut(task_id).ok_or(SchedulerError::UnknownTask)?;
        if task.state.is_terminal() {
            return Err(SchedulerError::InvalidAddressSpace);
        }
        task.address_space = address_space;
        Ok(())
    }

    /// Change a task's nice value (-20..=19, Linux weights). A runnable task
    /// keeps its virtual runtime; only its share and deadline change.
    pub fn set_nice(&mut self, task_id: TaskId, nice: i8) -> Result<(), SchedulerError> {
        let runnable = self.is_in_runnable_set(task_id);
        let task = self.task_mut(task_id).ok_or(SchedulerError::UnknownTask)?;
        if task.state.is_terminal() {
            return Err(SchedulerError::InvalidTransition);
        }
        let entity = &mut task.scheduler_entity;
        let old_weight = entity.weight as VirtualTime;
        entity.nice = nice.clamp(MIN_NICE, MAX_NICE);
        entity.weight = nice_to_weight(entity.nice);
        entity.virtual_deadline = entity.virtual_runtime + entity.weighted_slice();
        let (new_weight, runtime) = (entity.weight as VirtualTime, entity.virtual_runtime);
        if runnable {
            self.sum_weight += new_weight - old_weight;
            self.sum_weighted_runtime += (new_weight - old_weight) * runtime;
            self.refresh_average();
        }
        Ok(())
    }

    pub fn snapshots(&self) -> Vec<TaskSnapshot> {
        let average = self.average;
        self.tasks
            .iter()
            .flatten()
            .map(|task| TaskSnapshot {
                task_id: task.task_id,
                name: task.name,
                process_id: task.process_id,
                state: task.state,
                virtual_runtime: task.scheduler_entity.virtual_runtime,
                virtual_deadline: task.scheduler_entity.virtual_deadline,
                exec_runtime: task.scheduler_entity.exec_runtime,
                nice: task.scheduler_entity.nice,
                address_space: task.address_space,
                is_idle: task.is_idle,
                resume_kind: task.resume_kind,
                block_reason: task.block_reason,
                exit_status: task.exit_status,
                enqueue_time: task.scheduler_entity.enqueue_time,
                eligible: task.scheduler_entity.is_eligible(average),
                lag: if task.state.is_runnable() {
                    average - task.scheduler_entity.virtual_runtime
                } else {
                    task.scheduler_entity.lag
                },
            })
            .collect()
    }

    /// Timer hook, called on every tick with interrupts disabled. Charges the
    /// running task and reports whether it should be preempted: its request
    /// completed (EEVDF), its slice ran out (round-robin), a waking task has
    /// an earlier eligible deadline, or the idle task is running while work
    /// is queued.
    pub fn tick(&mut self, now: Nanos) -> bool {
        self.clock = now;
        let queued = !self.run_queue.is_empty();
        let Some(current) = self.current.filter(|current| Some(*current) != self.idle) else {
            self.stats.idle_ticks = self.stats.idle_ticks.saturating_add(1);
            return queued;
        };
        let request_completed = self.charge(current, now);
        let wakeup = core::mem::take(&mut self.wakeup_preempt);
        queued
            && match self.policy {
                PolicyKind::Fifo => false,
                PolicyKind::RoundRobin => self
                    .task(current)
                    .is_some_and(|task| now.saturating_sub(task.slice_start) >= BASE_SLICE_NS),
                PolicyKind::Eevdf => request_completed || wakeup,
            }
    }

    /// Put the running task back on the run queue (if still runnable) and
    /// dispatch the next one.
    pub fn schedule(&mut self, now: Nanos) -> Result<Option<TaskId>, SchedulerError> {
        self.clock = now;
        let previous = self.current;
        if let Some(current) = previous {
            let state = self.task(current).ok_or(SchedulerError::UnknownTask)?.state;
            if state == TaskState::Running {
                self.charge(current, now);
                let clock = self.clock;
                let task = self.task_mut(current).ok_or(SchedulerError::UnknownTask)?;
                task.transition(TaskState::Ready)?;
                if !task.is_idle {
                    task.scheduler_entity.enqueue_time = clock;
                    self.run_queue.push(current);
                }
            }
        }
        self.current = None;
        self.switch_to_next(previous, now)
    }

    /// Save the running task's interrupt frame and select the next task. The
    /// caller must release the scheduler lock before restoring the returned
    /// stack pointer.
    pub fn preempt(&mut self, stack_pointer: u64, now: Nanos) -> TaskId {
        let previous = self.current;
        if let Some(current) = previous
            && let Some(task) = self.task_mut(current)
        {
            task.interrupt_stack_pointer = stack_pointer;
            task.resume_kind = ResumeKind::Interrupt;
        }
        let _ = self.schedule(now);
        let next = self
            .current
            .or(self.idle)
            .expect("preemption has no runnable task");
        if Some(next) != previous {
            self.stats.timer_preemptions = self.stats.timer_preemptions.saturating_add(1);
        }
        next
    }

    pub fn yield_current(&mut self, now: Nanos) -> Result<Option<TaskId>, SchedulerError> {
        self.schedule(now)
    }

    pub fn prepare_voluntary_yield(
        &mut self,
        now: Nanos,
    ) -> Result<(TaskId, TaskId), SchedulerError> {
        let current = self.current.ok_or(SchedulerError::UnknownTask)?;
        let task = self.task_mut(current).ok_or(SchedulerError::UnknownTask)?;
        if task.state != TaskState::Running {
            return Err(SchedulerError::InvalidTransition);
        }
        task.resume_kind = ResumeKind::Voluntary;
        let next = self.schedule(now)?.ok_or(SchedulerError::UnknownTask)?;
        self.stats.voluntary_yields = self.stats.voluntary_yields.saturating_add(1);
        Ok((current, next))
    }

    pub fn sleep_current_until(
        &mut self,
        deadline: Tick,
        now: Nanos,
    ) -> Result<Option<TaskId>, SchedulerError> {
        let current = self.stop_current(TaskState::Blocked, now)?;
        let task = self.task_mut(current).ok_or(SchedulerError::UnknownTask)?;
        task.block_reason = Some(BlockReason::SleepUntil(deadline));
        let key = (deadline, current);
        let position = self.sleepers.partition_point(|entry| *entry > key);
        self.sleepers.insert(position, key);
        self.switch_to_next(Some(current), now)
    }

    /// Wake every sleeper whose deadline has passed. Allocation free; called
    /// from the timer interrupt.
    pub fn wake_expired(&mut self, now: Tick) -> usize {
        let mut woken = 0;
        while let Some(&(deadline, id)) = self.sleepers.last() {
            if deadline > now {
                break;
            }
            self.sleepers.pop();
            // Skip entries whose task was woken another way (or is sleeping
            // again with a different deadline).
            let still_sleeping = self.task(id).is_some_and(|task| {
                task.state == TaskState::Blocked
                    && task.block_reason == Some(BlockReason::SleepUntil(deadline))
            });
            if still_sleeping {
                self.make_ready(id);
                woken += 1;
            }
        }
        woken
    }

    pub fn block_current(
        &mut self,
        reason: BlockReason,
        wait_queue: &mut WaitQueue,
        now: Nanos,
    ) -> Result<Option<TaskId>, SchedulerError> {
        let current = self.stop_current(TaskState::Blocked, now)?;
        let task = self.task_mut(current).ok_or(SchedulerError::UnknownTask)?;
        task.block_reason = Some(reason);
        task.wait_link = Some(wait_queue.id);
        wait_queue.tasks.push_back(current);
        self.switch_to_next(Some(current), now)
    }

    pub fn wake_one(
        &mut self,
        wait_queue: &mut WaitQueue,
    ) -> Result<Option<TaskId>, SchedulerError> {
        let Some(id) = wait_queue.tasks.pop_front() else {
            return Ok(None);
        };
        let task = self.task(id).ok_or(SchedulerError::UnknownTask)?;
        if task.state != TaskState::Blocked {
            return Err(SchedulerError::NotBlocked);
        }
        self.make_ready(id);
        Ok(Some(id))
    }

    pub fn wake_all(&mut self, wait_queue: &mut WaitQueue) -> Result<usize, SchedulerError> {
        let mut count = 0;
        while self.wake_one(wait_queue)?.is_some() {
            count += 1;
        }
        Ok(count)
    }

    pub fn wake_task(&mut self, task_id: TaskId) -> Result<(), SchedulerError> {
        let task = self.task(task_id).ok_or(SchedulerError::UnknownTask)?;
        if task.state != TaskState::Blocked {
            return Err(SchedulerError::NotBlocked);
        }
        self.make_ready(task_id);
        Ok(())
    }

    pub fn cancel_wait(
        &mut self,
        task_id: TaskId,
        wait_queue: &mut WaitQueue,
    ) -> Result<(), SchedulerError> {
        let task = self.task(task_id).ok_or(SchedulerError::UnknownTask)?;
        if task.state != TaskState::Blocked || task.wait_link != Some(wait_queue.id) {
            return Err(SchedulerError::NotBlocked);
        }
        if !wait_queue.remove(task_id) {
            return Err(SchedulerError::NotBlocked);
        }
        self.make_ready(task_id);
        Ok(())
    }

    pub fn exit_current(
        &mut self,
        status: ExitStatus,
        now: Nanos,
    ) -> Result<Option<TaskId>, SchedulerError> {
        let current = self.stop_current(TaskState::Exited, now)?;
        let task = self.task_mut(current).ok_or(SchedulerError::UnknownTask)?;
        task.exit_status = Some(status);
        task.block_reason = None;
        self.exit_queue.push_back(current);
        self.switch_to_next(Some(current), now)
    }

    pub fn reap_one(&mut self) -> Result<Option<TaskControlBlock>, SchedulerError> {
        let Some(&id) = self.exit_queue.front() else {
            return Ok(None);
        };
        if self.current == Some(id) {
            return Err(SchedulerError::CannotReapCurrent);
        }
        self.exit_queue.pop_front();
        let slot = self.index.remove(&id).ok_or(SchedulerError::UnknownTask)?;
        let task = self.tasks[slot].take().ok_or(SchedulerError::UnknownTask)?;
        self.free_slots.push(slot);
        self.stats.tasks_reaped = self.stats.tasks_reaped.saturating_add(1);
        Ok(Some(*task))
    }

    /// Charge and remove the running task from the runnable set, moving it
    /// to `state` (blocked or exited). The caller then picks a successor.
    fn stop_current(&mut self, state: TaskState, now: Nanos) -> Result<TaskId, SchedulerError> {
        self.clock = now;
        let current = self.current.ok_or(SchedulerError::UnknownTask)?;
        let task = self.task(current).ok_or(SchedulerError::UnknownTask)?;
        if task.state != TaskState::Running {
            return Err(SchedulerError::InvalidTransition);
        }
        self.charge(current, now);
        self.leave(current);
        let task = self.task_mut(current).ok_or(SchedulerError::UnknownTask)?;
        task.resume_kind = ResumeKind::Voluntary;
        task.transition(state)?;
        self.current = None;
        Ok(current)
    }

    /// Blocked -> Ready, rejoining the runnable set with its saved lag.
    fn make_ready(&mut self, id: TaskId) {
        if let Some(task) = self.task_mut(id) {
            let _ = task.transition(TaskState::Ready);
            task.block_reason = None;
            task.wait_link = None;
        }
        self.join(id);
        self.stats.tasks_woken = self.stats.tasks_woken.saturating_add(1);
    }

    fn switch_to_next(
        &mut self,
        previous: Option<TaskId>,
        now: Nanos,
    ) -> Result<Option<TaskId>, SchedulerError> {
        self.wakeup_preempt = false;
        let next = self.pick_next();
        if let Some(id) = next {
            let task = self.task_mut(id).ok_or(SchedulerError::UnknownTask)?;
            task.transition(TaskState::Running)?;
            task.dispatched_at = now;
            task.slice_start = now;
        }
        self.current = next;
        if next != previous {
            self.stats.context_switches = self.stats.context_switches.saturating_add(1);
        }
        Ok(next)
    }

    /// Add a task that just became runnable with its saved lag intact.
    ///
    /// Inserting the task moves `V` toward it, so placing it at `V - lag`
    /// would leave only part of the lag. Scaling by `(W + w) / W` (as Linux
    /// does) makes `V' - vruntime == lag` after insertion.
    fn join(&mut self, id: TaskId) {
        let average = self.average;
        let clock = self.clock;
        let others = self.sum_weight;
        let Some(task) = self.task_mut(id) else {
            return;
        };
        let entity = &mut task.scheduler_entity;
        let weight = entity.weight as VirtualTime;
        let placed_lag = if others > 0 {
            entity.lag * (others + weight) / others
        } else {
            entity.lag
        };
        entity.virtual_runtime = average - placed_lag;
        entity.lag = 0;
        entity.virtual_deadline = entity.virtual_runtime + entity.weighted_slice();
        entity.enqueue_time = clock;
        let (weight, runtime, deadline) = (
            entity.weight as VirtualTime,
            entity.virtual_runtime,
            entity.virtual_deadline,
        );
        self.sum_weight += weight;
        self.sum_weighted_runtime += weight * runtime;
        self.refresh_average();
        self.run_queue.push(id);
        if let Some(current) = self.current
            && Some(current) != self.idle
            && runtime <= self.average
            && self
                .task(current)
                .is_some_and(|task| deadline < task.scheduler_entity.virtual_deadline)
        {
            self.wakeup_preempt = true;
        }
    }

    /// Remove a task that stopped being runnable, remembering its lag.
    fn leave(&mut self, id: TaskId) {
        let average = self.average;
        let Some(task) = self.task_mut(id) else {
            return;
        };
        let entity = &mut task.scheduler_entity;
        entity.lag = (average - entity.virtual_runtime).clamp(-LAG_LIMIT, LAG_LIMIT);
        let (weight, runtime) = (entity.weight as VirtualTime, entity.virtual_runtime);
        self.sum_weight -= weight;
        self.sum_weighted_runtime -= weight * runtime;
        self.refresh_average();
    }

    /// Charge CPU time used since the last charge. Returns whether the
    /// task's current request completed.
    fn charge(&mut self, id: TaskId, now: Nanos) -> bool {
        let Some(task) = self.task_mut(id) else {
            return false;
        };
        let ran = now.saturating_sub(task.dispatched_at);
        task.dispatched_at = now;
        if task.is_idle {
            return false;
        }
        let entity = &mut task.scheduler_entity;
        let before = entity.virtual_runtime;
        let completed = entity.account(ran);
        let weighted_delta = entity.weight as VirtualTime * (entity.virtual_runtime - before);
        self.sum_weighted_runtime += weighted_delta;
        self.refresh_average();
        completed
    }

    fn is_in_runnable_set(&self, id: TaskId) -> bool {
        self.task(id)
            .is_some_and(|task| !task.is_idle && task.state.is_runnable())
    }

    fn refresh_average(&mut self) {
        if self.sum_weight > 0 {
            self.average = self.sum_weighted_runtime / self.sum_weight;
        }
    }

    /// Remove and return the next task to run, or the idle task.
    fn pick_next(&mut self) -> Option<TaskId> {
        if self.run_queue.is_empty() {
            return self.idle;
        }
        let average = self.average;
        let mut best: Option<(usize, (VirtualTime, TaskId))> = None;
        let mut fallback: Option<(usize, (VirtualTime, TaskId))> = None;
        for (index, id) in self.run_queue.iter().enumerate() {
            let Some(task) = self.task(*id) else {
                continue;
            };
            let entity = &task.scheduler_entity;
            let key = match self.policy {
                PolicyKind::Eevdf => (entity.virtual_deadline, *id),
                PolicyKind::Fifo | PolicyKind::RoundRobin => {
                    (entity.enqueue_time as VirtualTime, *id)
                }
            };
            let eligible = self.policy != PolicyKind::Eevdf || entity.is_eligible(average);
            if eligible && best.is_none_or(|(_, best_key)| key < best_key) {
                best = Some((index, key));
            }
            // Someone is always eligible against a weighted average; keep a
            // least-runtime fallback against rounding at the boundary.
            let runtime_key = (entity.virtual_runtime, *id);
            if fallback.is_none_or(|(_, fallback_key)| runtime_key < fallback_key) {
                fallback = Some((index, runtime_key));
            }
        }
        let (index, _) = best.or(fallback)?;
        Some(self.run_queue.swap_remove(index))
    }

    /// Test hook: move a runnable task's virtual runtime while keeping the
    /// weighted sums consistent.
    #[cfg(test)]
    pub(super) fn set_virtual_runtime(&mut self, id: TaskId, runtime: VirtualTime) {
        let runnable = self.is_in_runnable_set(id);
        let task = self.task_mut(id).unwrap();
        let entity = &mut task.scheduler_entity;
        let weight = entity.weight as VirtualTime;
        let delta = runtime - entity.virtual_runtime;
        entity.virtual_runtime = runtime;
        entity.virtual_deadline = runtime + entity.weighted_slice();
        if runnable {
            self.sum_weighted_runtime += weight * delta;
            self.refresh_average();
        }
    }

    #[cfg(test)]
    pub(super) fn wakeup_preempt_pending(&self) -> bool {
        self.wakeup_preempt
    }
}
