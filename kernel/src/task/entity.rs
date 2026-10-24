use super::{Nanos, VirtualTime};

/// Per-task EEVDF state. Virtual time is in weighted nanoseconds: running for
/// `t` ns advances `virtual_runtime` by `t * NICE_0_WEIGHT / weight`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EevdfEntity {
    pub virtual_runtime: VirtualTime,
    pub virtual_deadline: VirtualTime,
    pub weight: u64,
    pub nice: i8,
    /// Real CPU time consumed.
    pub exec_runtime: Nanos,
    /// `V - virtual_runtime` when the task last left the runnable set;
    /// restored when it rejoins so sleeping neither earns nor loses credit.
    pub lag: VirtualTime,
    /// When the task last became runnable (for FIFO/round-robin order).
    pub enqueue_time: Nanos,
}

impl Default for EevdfEntity {
    fn default() -> Self {
        Self {
            virtual_runtime: 0,
            virtual_deadline: 0,
            weight: NICE_0_WEIGHT,
            nice: 0,
            exec_runtime: 0,
            lag: 0,
            enqueue_time: 0,
        }
    }
}

pub const NICE_0_WEIGHT: u64 = 1024;
/// Request size: how long a task runs before its deadline comes due.
pub const BASE_SLICE_NS: Nanos = 3_000_000;
/// Bound on carried lag, in virtual nanoseconds, so a long sleeper cannot
/// bank unbounded credit (or debt).
pub const LAG_LIMIT: VirtualTime = 2 * BASE_SLICE_NS as VirtualTime;

pub const MIN_NICE: i8 = -20;
pub const MAX_NICE: i8 = 19;

/// Linux's `sched_prio_to_weight`: each nice step is about a 10% CPU share
/// difference against a nice-0 task.
const NICE_TO_WEIGHT: [u64; 40] = [
    88761, 71755, 56483, 46273, 36291, 29154, 23254, 18705, 14949, 11916, 9548, 7620, 6100, 4904,
    3906, 3121, 2501, 1991, 1586, 1277, 1024, 820, 655, 526, 423, 335, 272, 215, 172, 137, 110, 87,
    70, 56, 45, 36, 29, 23, 18, 15,
];

pub const fn nice_to_weight(nice: i8) -> u64 {
    let clamped = if nice < MIN_NICE {
        MIN_NICE
    } else if nice > MAX_NICE {
        MAX_NICE
    } else {
        nice
    };
    NICE_TO_WEIGHT[(clamped - MIN_NICE) as usize]
}

impl EevdfEntity {
    /// Virtual length of one request at this task's weight.
    pub fn weighted_slice(&self) -> VirtualTime {
        BASE_SLICE_NS as VirtualTime * NICE_0_WEIGHT as VirtualTime
            / self.weight.max(1) as VirtualTime
    }

    /// Virtual time that `ran` real nanoseconds cost this task.
    pub fn virtual_delta(&self, ran: Nanos) -> VirtualTime {
        ran as VirtualTime * NICE_0_WEIGHT as VirtualTime / self.weight.max(1) as VirtualTime
    }

    /// Charge `ran` real nanoseconds. Returns true when the current request
    /// completed, in which case a new deadline has been issued.
    pub fn account(&mut self, ran: Nanos) -> bool {
        self.exec_runtime = self.exec_runtime.saturating_add(ran);
        self.virtual_runtime += self.virtual_delta(ran);
        if self.virtual_runtime >= self.virtual_deadline {
            self.virtual_deadline = self.virtual_runtime + self.weighted_slice();
            true
        } else {
            false
        }
    }

    pub fn is_eligible(&self, average: VirtualTime) -> bool {
        self.virtual_runtime <= average
    }
}
