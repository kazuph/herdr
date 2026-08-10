/-! Finite decisions already fixed by SPEC G1; this model adds no product policy. -/

inductive TwoPaneLayout where
  | horizontal
  | vertical
  deriving DecidableEq, Repr

def nextTwoPaneLayout : TwoPaneLayout -> TwoPaneLayout
  | .horizontal => .vertical
  | .vertical => .horizontal

inductive FinitePaneCount where
  | one | two | three | four | five
  deriving DecidableEq, Repr

def paneCount : FinitePaneCount -> Nat
  | .one => 1 | .two => 2 | .three => 3 | .four => 4 | .five => 5

def gridTopCount : FinitePaneCount -> Nat
  | .one => 1 | .two => 1 | .three => 2 | .four => 2 | .five => 3

def gridBottomCount : FinitePaneCount -> Nat
  | .one => 0 | .two => 1 | .three => 1 | .four => 2 | .five => 2

theorem two_pane_toggle_round_trip (layout : TwoPaneLayout) :
    nextTwoPaneLayout (nextTwoPaneLayout layout) = layout := by
  cases layout <;> rfl

theorem grid_rounding_preserves_pane_count (count : FinitePaneCount) :
    gridTopCount count + gridBottomCount count = paneCount count := by
  cases count <;> decide

theorem grid_top_is_never_smaller (count : FinitePaneCount) :
    gridBottomCount count <= gridTopCount count := by
  cases count <;> decide

/-! Positive split geometry uses Rust f32::round semantics: an exact half goes
to the first subtree, and the second subtree owns the residual cells.  The
finite domain is deliberately the boundary set used by the contract tests. -/

def roundHalfUp (axis numerator denominator : Nat) : Nat :=
  (2 * axis * numerator + denominator) / (2 * denominator)

def finiteAxes : List Nat := [1, 2, 3, 4, 5, 29, 30, 31, 89, 90, 91]

def finiteSplits : List (Nat × Nat) :=
  [(1, 2), (1, 3), (2, 3), (1, 4), (3, 4), (1, 5), (4, 5)]

def splitCases : List (Nat × Nat × Nat) :=
  finiteAxes.flatMap fun axis =>
    finiteSplits.map fun split => (axis, split.1, split.2)

def splitCaseIsSafe (entry : Nat × Nat × Nat) : Bool :=
  let (axis, numerator, denominator) := entry
  let first := roundHalfUp axis numerator denominator
  let second := axis - first
  denominator > 0 && numerator <= denominator && first <= axis && first + second == axis

theorem finite_split_rounding_preserves_residual :
    splitCases.all splitCaseIsSafe = true := by native_decide

theorem odd_half_cell_goes_to_first :
    roundHalfUp 29 1 2 = 15 ∧ roundHalfUp 31 1 2 = 16 := by native_decide

def sidebarBounds (minimum maximum : Nat) : Nat × Nat :=
  if minimum <= maximum then (minimum, maximum) else (18, 72)

theorem inverted_sidebar_bounds_fail_closed :
    sidebarBounds 50 30 = (18, 72) := by native_decide

inductive PacketDisposition where
  | sourceAccept | semanticPort | rejectHold | evidenceOnly
  deriving DecidableEq, Repr

def changesProduct : PacketDisposition -> Bool
  | .sourceAccept | .semanticPort => true
  | .rejectHold | .evidenceOnly => false

def requiresRollback : PacketDisposition -> Bool
  | .sourceAccept | .semanticPort => true
  | .rejectHold | .evidenceOnly => false

theorem product_change_iff_rollback (disposition : PacketDisposition) :
    changesProduct disposition = requiresRollback disposition := by
  cases disposition <;> rfl
