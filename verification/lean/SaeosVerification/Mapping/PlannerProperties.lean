import SaeosVerification.Mapping.Planner

namespace SaeosVerification

/-- The page-policy sizes are positive, with huge pages larger than base pages. -/
theorem page_bytes_positive (pageSize : PageSize) : 0 < pageBytes pageSize := by
  cases pageSize <;> simp [pageBytes]

theorem page_bytes_ordered : pageBytes .base < pageBytes .twoMiB ∧
    pageBytes .twoMiB < pageBytes .oneGiB := by
  simp [pageBytes]

theorem choose_page_size_positive (kind : MemoryKind) (physical remaining : Nat)
    (gigabytePages : Bool) :
    0 < pageBytes (choosePage kind physical remaining gigabytePages) := by
  cases choosePage kind physical remaining gigabytePages <;> simp [pageBytes]

/-- Identity mapping preserves the physical address below the modeled limit. -/
theorem identity_address_value (physical : Nat)
    (hBelow : physical < fourLevelLimit) :
    virtualAddress .identity physical = some physical := by
  simp [virtualAddress, hBelow]

/-- Direct mapping adds the kernel virtual base below the modeled limit. -/
theorem direct_address_value (physical : Nat)
    (hBelow : physical < fourLevelLimit) :
    virtualAddress .direct physical = some (kernelVirtualBase + physical) := by
  simp [virtualAddress, hBelow]

/-- Both virtual-address mappings reject the first address outside the limit. -/
theorem virtual_address_rejects_limit (mode : MappingMode) :
    virtualAddress mode fourLevelLimit = none := by
  simp [virtualAddress]

theorem virtual_address_rejects_above_limit (mode : MappingMode) (physical : Nat)
    (hAbove : fourLevelLimit ≤ physical) :
    virtualAddress mode physical = none := by
  simp [virtualAddress, Nat.not_lt.mpr hAbove]

/-- Every successfully constructed chunk records the policy's selected size. -/
theorem mapping_chunk_matches_policy
    (mode : MappingMode) (kind : MemoryKind) (physical remaining : Nat)
    (gigabytePages : Bool) (chunk : MappingChunk)
    (hChunk : mappingChunk mode kind physical remaining gigabytePages = some chunk) :
    chunk.physicalStart = physical ∧
      chunk.size = pageBytes (choosePage kind physical remaining gigabytePages) ∧
      chunk.pageSize = choosePage kind physical remaining gigabytePages := by
  cases hAddress : virtualAddress mode physical <;>
    simp [mappingChunk, hAddress] at hChunk ⊢
  cases hChunk
  exact ⟨rfl, rfl, rfl⟩

/-- A successfully built direct-mapped chunk uses the direct virtual address. -/
theorem mapping_chunk_direct_address
    (kind : MemoryKind) (physical remaining : Nat) (gigabytePages : Bool)
    (chunk : MappingChunk)
    (hChunk : mappingChunk .direct kind physical remaining gigabytePages = some chunk) :
    chunk.virtualStart = kernelVirtualBase + physical := by
  by_cases hBelow : physical < fourLevelLimit <;>
    simp [mappingChunk, virtualAddress, hBelow] at hChunk ⊢ <;>
    cases hChunk <;> rfl

/-- A successfully built identity-mapped chunk preserves its physical address. -/
theorem mapping_chunk_identity_address
    (kind : MemoryKind) (physical remaining : Nat) (gigabytePages : Bool)
    (chunk : MappingChunk)
    (hChunk : mappingChunk .identity kind physical remaining gigabytePages = some chunk) :
    chunk.virtualStart = physical := by
  by_cases hBelow : physical < fourLevelLimit <;>
    simp [mappingChunk, virtualAddress, hBelow] at hChunk ⊢ <;>
    cases hChunk <;> rfl

end SaeosVerification
