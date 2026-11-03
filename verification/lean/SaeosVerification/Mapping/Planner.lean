import SaeosVerification.Mapping.Properties

namespace SaeosVerification

/-- The limit used by the four-level x86-64 mapping model. -/
def fourLevelLimit : Nat := 2 ^ 47

/-- Base of the kernel's direct physical-memory mapping. -/
def kernelVirtualBase : Nat := 0xffff800000000000

inductive MappingMode where
  | direct
  | identity
  deriving DecidableEq, Repr

/-- Convert a physical address according to a mapping mode, rejecting addresses
at or above the four-level limit. -/
def virtualAddress (mode : MappingMode) (physical : Nat) : Option Nat :=
  if physical < fourLevelLimit then
    some (match mode with
      | .direct => kernelVirtualBase + physical
      | .identity => physical)
  else
    none

structure MappingChunk where
  physicalStart : Nat
  virtualStart : Nat
  size : Nat
  pageSize : PageSize
  deriving DecidableEq, Repr

/-- Construct one chunk using the page-selection policy. The caller supplies a
physical cursor and the bytes remaining in the range. -/
def mappingChunk (mode : MappingMode) (kind : MemoryKind)
    (physical remaining : Nat) (gigabytePages : Bool) : Option MappingChunk := do
  let virtualStart ← virtualAddress mode physical
  let pageSize := choosePage kind physical remaining gigabytePages
  pure {
    physicalStart := physical
    virtualStart := virtualStart
    size := pageBytes pageSize
    pageSize := pageSize
  }

/-- A fuel-bounded range walk. Fuel is measured in bytes, so every valid page
selection can consume at least one unit; the valid page-aligned input domain
uses `end - start` as sufficient fuel. -/
def walkRange (fuel physical rangeEnd : Nat) (mode : MappingMode)
    (kind : MemoryKind) (gigabytePages : Bool) : List MappingChunk :=
  match fuel with
  | 0 => []
  | fuel' + 1 =>
      if physical < rangeEnd then
        match mappingChunk mode kind physical (rangeEnd - physical) gigabytePages with
        | none => []
        | some chunk =>
            chunk :: walkRange fuel' (physical + chunk.size) rangeEnd mode kind gigabytePages
      else
        []

def planRange (start rangeEnd : Nat) (mode : MappingMode)
    (kind : MemoryKind) (gigabytePages : Bool) : List MappingChunk :=
  walkRange (rangeEnd - start) start rangeEnd mode kind gigabytePages

/-- Every successfully constructed chunk starts at the supplied physical cursor. -/
theorem mappingChunk_starts_at
    (mode : MappingMode) (kind : MemoryKind) (physical remaining : Nat)
    (gigabytePages : Bool) (chunk : MappingChunk)
    (hChunk : mappingChunk mode kind physical remaining gigabytePages = some chunk) :
    chunk.physicalStart = physical := by
  cases hAddress : virtualAddress mode physical <;>
    simp [mappingChunk, hAddress] at hChunk ⊢
  cases hChunk
  rfl

/-- A list is an exact contiguous cover when its chunks start at the cursor,
advance it by a positive in-range size, and finish exactly at the endpoint. -/
def ExactCoverage (chunks : List MappingChunk) (start rangeEnd : Nat) : Prop :=
  match chunks with
  | [] => start = rangeEnd
  | chunk :: rest =>
      chunk.physicalStart = start ∧
      0 < chunk.size ∧
      start + chunk.size ≤ rangeEnd ∧
      ExactCoverage rest (start + chunk.size) rangeEnd

/-- Under a caller-supplied proof that every cursor has a positive chunk which
fits in the remaining range, the fuel-bounded walker exactly covers that range.
The precondition also makes explicit that address conversion never fails. -/
theorem walkRange_exactCoverage
    (fuel start rangeEnd : Nat) (mode : MappingMode)
    (kind : MemoryKind) (gigabytePages : Bool)
    (hFuel : rangeEnd - start ≤ fuel)
    (hBounds : start ≤ rangeEnd)
    (hSafe : ∀ cursor, start ≤ cursor → cursor < rangeEnd →
      ∃ chunk, mappingChunk mode kind cursor (rangeEnd - cursor) gigabytePages = some chunk ∧
        0 < chunk.size ∧ cursor + chunk.size ≤ rangeEnd) :
    ExactCoverage
      (walkRange fuel start rangeEnd mode kind gigabytePages) start rangeEnd := by
  induction fuel generalizing start with
  | zero =>
      have hEqual : start = rangeEnd := by omega
      simp [walkRange, ExactCoverage, hEqual]
  | succ fuel ih =>
      by_cases hLess : start < rangeEnd
      · obtain ⟨chunk, hChunk, hPositive, hFits⟩ := hSafe start (by omega) hLess
        simp only [walkRange, if_pos hLess, hChunk]
        simp only [ExactCoverage]
        refine ⟨mappingChunk_starts_at mode kind start (rangeEnd - start) gigabytePages chunk hChunk,
          hPositive, hFits, ?_⟩
        apply ih (start := start + chunk.size)
        · omega
        · omega
        · intro cursor hStart hCursor
          exact hSafe cursor (by omega) hCursor
      · have hEqual : start = rangeEnd := by omega
        simp [walkRange, ExactCoverage, hEqual]

theorem planRange_exactCoverage
    (start rangeEnd : Nat) (mode : MappingMode)
    (kind : MemoryKind) (gigabytePages : Bool)
    (hStart : start ≤ rangeEnd)
    (hSafe : ∀ cursor, start ≤ cursor → cursor < rangeEnd →
      ∃ chunk, mappingChunk mode kind cursor (rangeEnd - cursor) gigabytePages = some chunk ∧
        0 < chunk.size ∧ cursor + chunk.size ≤ rangeEnd) :
    ExactCoverage (planRange start rangeEnd mode kind gigabytePages) start rangeEnd := by
  apply walkRange_exactCoverage (fuel := rangeEnd - start)
  · omega
  · exact hStart
  · exact hSafe

end SaeosVerification
