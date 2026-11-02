namespace SaeosVerification

/-- Abstract memory kinds used by the mapping-policy model. -/
inductive MemoryKind where
  | ordinary
  | mmio
  deriving DecidableEq, Repr

/-- Leaf page sizes considered by the x86-64 mapping policy. -/
inductive PageSize where
  | oneGiB
  | twoMiB
  | base
  deriving DecidableEq, Repr

def pageBytes : PageSize → Nat
  | .oneGiB => 1073741824
  | .twoMiB => 2097152
  | .base => 4096

def choosePage (kind : MemoryKind) (physical remaining : Nat) (gigabytePages : Bool) : PageSize :=
  if kind = .mmio then
    .base
  else if gigabytePages = true ∧ physical % pageBytes .oneGiB = 0 ∧ remaining ≥ pageBytes .oneGiB then
    .oneGiB
  else if physical % pageBytes .twoMiB = 0 ∧ remaining ≥ pageBytes .twoMiB then
    .twoMiB
  else
    .base

/-- MMIO is always mapped with 4 KiB leaves, independent of alignment or range length. -/
theorem mmio_uses_base_pages (physical remaining : Nat) (gigabytePages : Bool) :
    choosePage .mmio physical remaining gigabytePages = .base := by
  simp [choosePage]

/-- An aligned, sufficiently large ordinary range selects a 1 GiB leaf
when gigabyte pages are enabled. -/
theorem aligned_gigabyte_uses_huge_page (physical remaining : Nat)
    (hAligned : physical % pageBytes .oneGiB = 0)
    (hEnough : remaining ≥ pageBytes .oneGiB) :
    choosePage .ordinary physical remaining true = .oneGiB := by
  simp [choosePage, hAligned, hEnough]

end SaeosVerification
