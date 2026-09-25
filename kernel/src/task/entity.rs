use super::{Tick, VirtualTime};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EevdfEntity {
    pub virtual_runtime: VirtualTime,
    pub virtual_deadline: VirtualTime,
    pub requested_slice: Tick,
    pub weight: u64,
    pub exec_runtime: Tick,
    pub eligible: bool,
    pub lag: VirtualTime,
    pub enqueue_time: Tick,
}

impl Default for EevdfEntity {
    fn default() -> Self {
        Self {
            virtual_runtime: 0,
            virtual_deadline: 0,
            requested_slice: TARGET_LATENCY,
            weight: NICE_0_WEIGHT,
            exec_runtime: 0,
            eligible: true,
            lag: 0,
            enqueue_time: 0,
        }
    }
}

pub const NICE_0_WEIGHT: u64 = 1024;
pub const TARGET_LATENCY: Tick = 20;
pub const MIN_GRANULARITY: Tick = 1;

impl EevdfEntity {
    /// Simplified EEVDF enqueue placement. This is intentionally an
    /// experiment-friendly model, not a claim to implement every detail of
    /// any production scheduler.
    pub fn enqueue(&mut self, scheduler_time: VirtualTime, runnable_count: usize, now: Tick) {
        self.virtual_runtime = self.virtual_runtime.max(scheduler_time);
        self.lag = scheduler_time.saturating_sub(self.virtual_runtime);
        self.eligible = self.lag >= 0;
        self.enqueue_time = now;
        let count = runnable_count.max(1) as Tick;
        self.requested_slice = (TARGET_LATENCY / count).clamp(MIN_GRANULARITY, TARGET_LATENCY);
        let weighted_slice = (self.requested_slice as VirtualTime)
            .saturating_mul(NICE_0_WEIGHT as VirtualTime)
            / self.weight.max(1) as VirtualTime;
        self.virtual_deadline = self.virtual_runtime.saturating_add(weighted_slice);
    }

    pub fn account(&mut self, elapsed: Tick) {
        self.exec_runtime = self.exec_runtime.saturating_add(elapsed);
        let delta = (elapsed as VirtualTime).saturating_mul(NICE_0_WEIGHT as VirtualTime)
            / self.weight.max(1) as VirtualTime;
        self.virtual_runtime = self.virtual_runtime.saturating_add(delta);
    }

    pub(super) fn refresh_deadline(&mut self, runnable_count: usize) {
        let count = runnable_count.max(1) as Tick;
        self.requested_slice = (TARGET_LATENCY / count).clamp(MIN_GRANULARITY, TARGET_LATENCY);
        let weighted_slice = (self.requested_slice as VirtualTime)
            .saturating_mul(NICE_0_WEIGHT as VirtualTime)
            / self.weight.max(1) as VirtualTime;
        self.virtual_deadline = self.virtual_runtime.saturating_add(weighted_slice);
    }
}
