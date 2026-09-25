use super::*;
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

pub struct Scheduler {
    tasks: Vec<TaskControlBlock>,
    run_queue: Vec<TaskId>,
    exit_queue: VecDeque<TaskId>,
    next_task_id: u64,
    current: Option<TaskId>,
    idle: Option<TaskId>,
    sleepers: Vec<(Tick, TaskId)>,
    virtual_time: VirtualTime,
    clock: Tick,
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
            run_queue: Vec::new(),
            exit_queue: VecDeque::new(),
            next_task_id: 1,
            current: None,
            idle: None,
            sleepers: Vec::new(),
            virtual_time: 0,
            clock: 0,
            policy: PolicyKind::Eevdf,
            stats: SchedulerStats {
                context_switches: 0,
                timer_preemptions: 0,
                voluntary_yields: 0,
                tasks_woken: 0,
                tasks_reaped: 0,
            },
        }
    }

    pub fn with_policy(policy: PolicyKind) -> Self {
        let mut scheduler = Self::new();
        scheduler.policy = policy;
        scheduler
    }

    pub fn spawn(&mut self, kernel_stack: KernelStack, name: &'static str) -> TaskId {
        let id = TaskId(self.next_task_id);
        self.next_task_id = self.next_task_id.saturating_add(1);
        let mut task = TaskControlBlock::new(id, kernel_stack, name);
        task.transition(TaskState::Ready)
            .expect("new task transition");
        self.tasks.push(task);
        self.enqueue(id).expect("new task must be enqueueable");
        id
    }

    /// Create a task with an interrupt-return frame prepared on its private
    /// kernel stack. Runtime tasks use this path so the timer entry can switch
    /// directly between task-owned interrupt frames.
    pub fn spawn_kernel_task(
        &mut self,
        entry: TaskEntry,
        name: &'static str,
        is_idle: bool,
    ) -> TaskId {
        let id = TaskId(self.next_task_id);
        self.next_task_id = self.next_task_id.saturating_add(1);
        let mut storage = vec![0u8; DEFAULT_KERNEL_STACK_SIZE].into_boxed_slice();
        let base = storage.as_mut_ptr() as u64;
        let top = (base + storage.len() as u64) & !0xf;
        #[cfg(any(target_os = "none", target_os = "uefi"))]
        let bootstrap_ip = crate::arch::x86_64::context::task_bootstrap_ip();
        #[cfg(not(any(target_os = "none", target_os = "uefi")))]
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
        if is_idle {
            self.idle = Some(id);
            self.tasks.push(task);
        } else {
            self.tasks.push(task);
            self.enqueue(id)
                .expect("new kernel task must be enqueueable");
        }
        id
    }

    pub fn current(&self) -> Option<TaskId> {
        self.current
    }

    pub fn task(&self, id: TaskId) -> Option<&TaskControlBlock> {
        self.tasks.iter().find(|task| task.task_id == id)
    }

    pub fn task_mut(&mut self, id: TaskId) -> Option<&mut TaskControlBlock> {
        self.tasks.iter_mut().find(|task| task.task_id == id)
    }

    pub fn runnable_count(&self) -> usize {
        self.run_queue.len() + usize::from(self.current.is_some())
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

    pub fn snapshots(&self) -> Vec<TaskSnapshot> {
        self.tasks
            .iter()
            .map(|task| TaskSnapshot {
                task_id: task.task_id,
                name: task.name,
                process_id: task.process_id,
                state: task.state,
                virtual_runtime: task.scheduler_entity.virtual_runtime,
                virtual_deadline: task.scheduler_entity.virtual_deadline,
                exec_runtime: task.scheduler_entity.exec_runtime,
                address_space: task.address_space,
                is_idle: task.is_idle,
                resume_kind: task.resume_kind,
                block_reason: task.block_reason,
                exit_status: task.exit_status,
                enqueue_time: task.scheduler_entity.enqueue_time,
                eligible: self
                    .virtual_time
                    .saturating_sub(task.scheduler_entity.virtual_runtime)
                    >= 0,
                lag: self
                    .virtual_time
                    .saturating_sub(task.scheduler_entity.virtual_runtime),
            })
            .collect()
    }

    pub fn schedule(&mut self, elapsed: Tick) -> Result<Option<TaskId>, SchedulerError> {
        self.clock = self.clock.saturating_add(elapsed);
        if let Some(current) = self.current {
            let task = self.task_mut(current).ok_or(SchedulerError::UnknownTask)?;
            if task.state == TaskState::Running {
                task.scheduler_entity.account(elapsed);
                task.transition(TaskState::Ready)?;
                if !task.is_idle {
                    self.enqueue(current)?;
                }
            }
        }

        let next = self.pick_next();
        self.current = next;
        if let Some(id) = next {
            self.task_mut(id)
                .ok_or(SchedulerError::UnknownTask)?
                .transition(TaskState::Running)?;
        }
        Ok(next)
    }

    /// Save the current task's interrupt-frame stack pointer and select the
    /// next task. The caller must release the scheduler lock before restoring
    /// the returned stack pointer.
    pub fn preempt(&mut self, stack_pointer: u64, elapsed: Tick) -> TaskId {
        if self.policy == PolicyKind::Fifo {
            if let Some(current) = self.current {
                if let Some(task) = self.task_mut(current) {
                    task.interrupt_stack_pointer = stack_pointer;
                    task.resume_kind = ResumeKind::Interrupt;
                    task.scheduler_entity.account(elapsed);
                }
                self.clock = self.clock.saturating_add(elapsed);
                return current;
            }
        }
        self.stats.timer_preemptions = self.stats.timer_preemptions.saturating_add(1);
        self.stats.context_switches = self.stats.context_switches.saturating_add(1);
        if let Some(current) = self.current {
            if let Some(task) = self.task_mut(current) {
                task.interrupt_stack_pointer = stack_pointer;
                task.resume_kind = ResumeKind::Interrupt;
            }
            let _ = self.schedule(elapsed);
        } else if let Some(next) = self.pick_next() {
            self.current = Some(next);
            if let Some(task) = self.task_mut(next) {
                let _ = task.transition(TaskState::Running);
            }
        }
        self.current.unwrap_or_else(|| {
            let _ = stack_pointer;
            self.idle.expect("preemption has no runnable task")
        })
    }

    pub fn yield_current(&mut self) -> Result<Option<TaskId>, SchedulerError> {
        self.schedule(0)
    }

    pub fn prepare_voluntary_yield(&mut self) -> Result<(TaskId, TaskId), SchedulerError> {
        let current = self.current.ok_or(SchedulerError::UnknownTask)?;
        let task = self.task_mut(current).ok_or(SchedulerError::UnknownTask)?;
        if task.state != TaskState::Running {
            return Err(SchedulerError::InvalidTransition);
        }
        task.resume_kind = ResumeKind::Voluntary;
        task.transition(TaskState::Ready)?;
        if !task.is_idle {
            self.enqueue(current)?;
        }
        let next = self.pick_next().ok_or(SchedulerError::UnknownTask)?;
        self.stats.voluntary_yields = self.stats.voluntary_yields.saturating_add(1);
        self.stats.context_switches = self.stats.context_switches.saturating_add(1);
        self.current = Some(next);
        self.task_mut(next)
            .ok_or(SchedulerError::UnknownTask)?
            .transition(TaskState::Running)?;
        Ok((current, next))
    }

    pub fn sleep_current_until(
        &mut self,
        deadline: Tick,
    ) -> Result<Option<TaskId>, SchedulerError> {
        let current = self.current.ok_or(SchedulerError::UnknownTask)?;
        let task = self.task_mut(current).ok_or(SchedulerError::UnknownTask)?;
        if task.state != TaskState::Running {
            return Err(SchedulerError::InvalidTransition);
        }
        task.resume_kind = ResumeKind::Voluntary;
        task.transition(TaskState::Blocked)?;
        task.block_reason = Some(BlockReason::SleepUntil(deadline));
        self.sleepers.push((deadline, current));
        self.sleepers.sort_by_key(|(wake_at, id)| (*wake_at, *id));
        self.current = None;
        self.schedule(0)
    }

    pub fn wake_expired(&mut self, now: Tick) -> usize {
        let mut woken = 0;
        let mut index = 0;
        while index < self.sleepers.len() {
            if self.sleepers[index].0 > now {
                index += 1;
                continue;
            }
            let (_, id) = self.sleepers.swap_remove(index);
            let should_wake = self.task(id).is_some_and(|task| {
                task.state == TaskState::Blocked
                    && task
                        .block_reason
                        .is_some_and(|reason| matches!(reason, BlockReason::SleepUntil(_)))
            });
            if should_wake {
                if let Some(task) = self.task_mut(id) {
                    let _ = task.transition(TaskState::Ready);
                    task.block_reason = None;
                }
                let _ = self.enqueue(id);
                self.stats.tasks_woken = self.stats.tasks_woken.saturating_add(1);
                woken += 1;
            }
        }
        woken
    }

    pub fn block_current(
        &mut self,
        reason: BlockReason,
        wait_queue: &mut WaitQueue,
    ) -> Result<Option<TaskId>, SchedulerError> {
        let current = self.current.ok_or(SchedulerError::UnknownTask)?;
        let task = self.task_mut(current).ok_or(SchedulerError::UnknownTask)?;
        if task.state != TaskState::Running {
            return Err(SchedulerError::InvalidTransition);
        }
        task.resume_kind = ResumeKind::Voluntary;
        task.transition(TaskState::Blocked)?;
        task.block_reason = Some(reason);
        task.wait_link = Some(wait_queue.id);
        wait_queue.tasks.push_back(current);
        self.current = None;
        self.schedule(0)
    }

    pub fn wake_one(
        &mut self,
        wait_queue: &mut WaitQueue,
    ) -> Result<Option<TaskId>, SchedulerError> {
        let Some(id) = wait_queue.tasks.pop_front() else {
            return Ok(None);
        };
        let task = self.task_mut(id).ok_or(SchedulerError::UnknownTask)?;
        if task.state != TaskState::Blocked {
            return Err(SchedulerError::NotBlocked);
        }
        task.transition(TaskState::Ready)?;
        task.block_reason = None;
        task.wait_link = None;
        self.enqueue(id)?;
        self.stats.tasks_woken = self.stats.tasks_woken.saturating_add(1);
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
        let task = self.task_mut(task_id).ok_or(SchedulerError::UnknownTask)?;
        if task.state != TaskState::Blocked {
            return Err(SchedulerError::NotBlocked);
        }
        task.transition(TaskState::Ready)?;
        task.block_reason = None;
        task.wait_link = None;
        self.enqueue(task_id)?;
        self.stats.tasks_woken = self.stats.tasks_woken.saturating_add(1);
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
        let task = self.task_mut(task_id).ok_or(SchedulerError::UnknownTask)?;
        task.transition(TaskState::Ready)?;
        task.block_reason = None;
        task.wait_link = None;
        self.enqueue(task_id)
    }

    pub fn exit_current(&mut self, status: ExitStatus) -> Result<Option<TaskId>, SchedulerError> {
        let current = self.current.ok_or(SchedulerError::UnknownTask)?;
        let task = self.task_mut(current).ok_or(SchedulerError::UnknownTask)?;
        if task.state != TaskState::Running {
            return Err(SchedulerError::InvalidTransition);
        }
        task.transition(TaskState::Exited)?;
        task.exit_status = Some(status);
        task.block_reason = None;
        self.exit_queue.push_back(current);
        self.current = None;
        self.schedule(0)
    }

    pub fn reap_one(&mut self) -> Result<Option<TaskControlBlock>, SchedulerError> {
        let Some(id) = self.exit_queue.pop_front() else {
            return Ok(None);
        };
        if self.current == Some(id) {
            return Err(SchedulerError::CannotReapCurrent);
        }
        let index = self
            .tasks
            .iter()
            .position(|task| task.task_id == id)
            .ok_or(SchedulerError::UnknownTask)?;
        self.stats.tasks_reaped = self.stats.tasks_reaped.saturating_add(1);
        Ok(Some(self.tasks.swap_remove(index)))
    }

    fn enqueue(&mut self, id: TaskId) -> Result<(), SchedulerError> {
        if self.run_queue.contains(&id) {
            return Err(SchedulerError::AlreadyQueued);
        }
        let runnable_count = self.run_queue.len() + 1;
        let virtual_time = self.virtual_time;
        let clock = self.clock;
        let task = self.task_mut(id).ok_or(SchedulerError::UnknownTask)?;
        task.scheduler_entity
            .enqueue(virtual_time, runnable_count, clock);
        self.run_queue.push(id);
        let runnable_count = self.run_queue.len();
        let queued = self.run_queue.clone();
        for queued_id in queued {
            if let Some(task) = self.task_mut(queued_id) {
                task.scheduler_entity.refresh_deadline(runnable_count);
            }
        }
        self.recompute_virtual_time();
        Ok(())
    }

    fn pick_next(&mut self) -> Option<TaskId> {
        if self.run_queue.is_empty() {
            return self.idle;
        }
        self.recompute_virtual_time();
        let selected_index = self
            .run_queue
            .iter()
            .enumerate()
            .filter_map(|(index, id)| {
                let task = self.task(*id)?;
                let eligible = match self.policy {
                    PolicyKind::Eevdf => {
                        self.virtual_time
                            .saturating_sub(task.scheduler_entity.virtual_runtime)
                            >= 0
                    }
                    PolicyKind::Fifo | PolicyKind::RoundRobin => true,
                };
                eligible.then_some((index, task.scheduler_entity.virtual_deadline, task.task_id))
            })
            .min_by_key(|(_, deadline, id)| match self.policy {
                PolicyKind::Eevdf => (*deadline, *id),
                PolicyKind::Fifo | PolicyKind::RoundRobin => self
                    .task(*id)
                    .map(|task| (task.scheduler_entity.enqueue_time as VirtualTime, *id))
                    .unwrap_or((VirtualTime::MAX, *id)),
            })
            .map(|(index, _, _)| index)
            .or_else(|| {
                self.run_queue
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, id)| {
                        self.task(**id)
                            .map(|task| task.scheduler_entity.virtual_runtime)
                    })
                    .map(|(index, _)| index)
            })?;
        Some(self.run_queue.swap_remove(selected_index))
    }

    fn recompute_virtual_time(&mut self) {
        if self.run_queue.is_empty() {
            return;
        }
        let total: VirtualTime = self
            .run_queue
            .iter()
            .filter_map(|id| self.task(*id))
            .map(|task| task.scheduler_entity.virtual_runtime)
            .sum();
        self.virtual_time = total / self.run_queue.len() as VirtualTime;
        let virtual_time = self.virtual_time;
        let queued = self.run_queue.clone();
        for id in queued {
            if let Some(task) = self.task_mut(id) {
                let entity = &mut task.scheduler_entity;
                entity.lag = virtual_time.saturating_sub(entity.virtual_runtime);
                entity.eligible = entity.lag >= 0;
            }
        }
    }
}
