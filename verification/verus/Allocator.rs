use vstd::prelude::*;

verus! {

/// Minimal executable allocator accounting model. This is a model proof, not
/// verification of SaeOS's production buddy allocator.
struct AllocatorState {
    total: u64,
    allocated: u64,
}

/// Allocate one unit while preserving the accounting invariant.
fn allocate_one(state: &mut AllocatorState)
    requires
        old(state).allocated < old(state).total,
    ensures
        final(state).total == old(state).total,
        final(state).allocated == old(state).allocated + 1,
        final(state).allocated <= final(state).total,
{
    state.allocated = state.allocated + 1;
}

/// Release one allocated unit while preserving the accounting invariant.
fn release_one(state: &mut AllocatorState)
    requires
        old(state).allocated > 0,
        old(state).allocated <= old(state).total,
    ensures
        final(state).total == old(state).total,
        final(state).allocated + 1 == old(state).allocated,
        final(state).allocated <= final(state).total,
{
    state.allocated = state.allocated - 1;
}

} // verus!
