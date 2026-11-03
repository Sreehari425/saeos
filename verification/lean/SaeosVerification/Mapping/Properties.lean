import SaeosVerification.Mapping

namespace SaeosVerification

/-- Gigabyte pages cannot be selected when that capability is disabled. -/
theorem disabled_gigabyte_pages_not_selected (physical remaining : Nat)
    (hDisabled : gigabytePages = false) :
    choosePage .ordinary physical remaining gigabytePages ≠ .oneGiB := by
  simp [choosePage, hDisabled]
  split <;> simp

/-- A qualifying 2 MiB range uses a 2 MiB page when gigabyte pages are disabled. -/
theorem aligned_two_mib_uses_huge_page_when_gigabyte_disabled
    (physical remaining : Nat)
    (hAligned : physical % pageBytes .twoMiB = 0)
    (hEnough : remaining ≥ pageBytes .twoMiB) :
    choosePage .ordinary physical remaining false = .twoMiB := by
  simp [choosePage, hAligned, hEnough]

/-- A qualifying 2 MiB range is selected when gigabyte-page eligibility fails. -/
theorem aligned_two_mib_selected_without_gigabyte_eligibility
    (physical remaining : Nat) (gigabytePages : Bool)
    (hAlignedTwo : physical % pageBytes .twoMiB = 0)
    (hEnoughTwo : remaining ≥ pageBytes .twoMiB)
    (hNoGigabyte : ¬ (gigabytePages = true ∧
      physical % pageBytes .oneGiB = 0 ∧
      remaining ≥ pageBytes .oneGiB)) :
    choosePage .ordinary physical remaining gigabytePages = .twoMiB := by
  simp [choosePage, hAlignedTwo, hEnoughTwo, hNoGigabyte]

/-- When neither huge-page condition qualifies, ordinary memory uses base pages. -/
theorem ordinary_uses_base_pages_without_huge_page_eligibility
    (physical remaining : Nat) (gigabytePages : Bool)
    (hNoGigabyte : ¬ (gigabytePages = true ∧
      physical % pageBytes .oneGiB = 0 ∧
      remaining ≥ pageBytes .oneGiB))
    (hNoTwoMiB : ¬ (physical % pageBytes .twoMiB = 0 ∧
      remaining ≥ pageBytes .twoMiB)) :
    choosePage .ordinary physical remaining gigabytePages = .base := by
  simp [choosePage, hNoGigabyte, hNoTwoMiB]

/-- When both large-page sizes qualify, the policy prefers the 1 GiB page. -/
theorem gigabyte_page_has_priority (physical remaining : Nat)
    (hAlignedGigabyte : physical % pageBytes .oneGiB = 0)
    (hEnoughGigabyte : remaining ≥ pageBytes .oneGiB) :
    choosePage .ordinary physical remaining true = .oneGiB := by
  simp [choosePage, hAlignedGigabyte, hEnoughGigabyte]

end SaeosVerification
