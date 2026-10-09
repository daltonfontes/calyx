/-!
# Calyx's effect rules, mechanized

The core of `docs/paper/formal.md` and §4.1 of the paper, in Lean 4 (core
only, no Mathlib). Check with `lean formal/Effects.lean`.

Two protocols, each a transition system in which every choice the world can
make is a separate step: a request lost, an answer lost, a crash of the
process or of the machine, what a `verify` read finds, what a person decides.

* `WriteOnce`: one `write once` call (rules R3, R4 and D1). Theorems T1 (at
  most once), T2 (exactly once when finished with `verify` or `pause`) and
  T3 (nothing done is redone).
* `Keyed`: one `write` with an idempotency key taken from a model's answer
  (rules R1 and R2). Theorem: the service applies at most one key.

The hypotheses of the paper appear as follows. H1 (the journal honours
`fsync`): what is synced is a field that no crash step touches. H2 (the
service honours the key): the service step adds a key only once. H3 (a fresh
`verify`): the `verify` steps read the true count. H4 (no redelivery): a send
applies at most once. H5 (the person decides by what happened): the person
steps read the true count. Two counterexamples at the end show what breaks
when the runtime does not sync: in the broken protocols the same world makes
the effect happen twice.
-/

namespace Calyx

/-! ## `write once` -/

/-- The state of one `write once` call, as the runtime and the world see it. -/
structure WriteOnce where
  /-- `begin(κ)` is in the journal, on disk (synced before any send: R3). -/
  begun : Bool
  /-- `done(κ)` is in the journal, on disk. -/
  done : Bool
  /-- Taken as done by `accept_loss`, without knowing whether it happened. -/
  lost : Bool
  /-- The runtime holds that the call did not happen, and may send it. -/
  permit : Bool
  /-- How many times the service applied the call (the truth). -/
  applied : Nat
  deriving DecidableEq, Repr

def WriteOnce.init : WriteOnce :=
  { begun := false, done := false, lost := false, permit := false, applied := 0 }

/-- One step of the runtime or of the world. -/
inductive WriteOnce.Step : WriteOnce → WriteOnce → Prop
  /-- R3: record `begin` (synced), then the call may be sent. -/
  | start (s : WriteOnce) :
      s.begun = false → s.done = false →
      Step s { s with begun := true, permit := true }
  /-- The request is applied and the answer arrives: `done` is recorded. -/
  | sendOk (s : WriteOnce) :
      s.permit = true → s.done = false →
      Step s { s with applied := s.applied + 1, done := true, permit := false }
  /-- The request is lost: nothing applied; the runtime sees a timeout. -/
  | sendLost (s : WriteOnce) :
      s.permit = true →
      Step s { s with permit := false }
  /-- The request is applied but the answer is lost: a timeout too. -/
  | sendNoAnswer (s : WriteOnce) :
      s.permit = true →
      Step s { s with applied := s.applied + 1, permit := false }
  /-- A crash, of the process or the machine: the runtime forgets everything
  but the journal on disk (H1). On resume, `begin` without `done` is uncertain. -/
  | crash (s : WriteOnce) :
      Step s { s with permit := false }
  /-- D1, `verify` finds the call (H3: it reads the truth). -/
  | verifyFound (s : WriteOnce) :
      s.begun = true → s.done = false → s.permit = false → s.applied > 0 →
      Step s { s with done := true }
  /-- D1, `verify` does not find it (H3), so it did not happen: send again. -/
  | verifyAbsent (s : WriteOnce) :
      s.begun = true → s.done = false → s.permit = false → s.applied = 0 →
      Step s { s with permit := true }
  /-- D1, `pause`: the person says it happened (H5). -/
  | personDone (s : WriteOnce) :
      s.begun = true → s.done = false → s.permit = false → s.applied > 0 →
      Step s { s with done := true }
  /-- D1, `pause`: the person says it did not happen (H5): send again. -/
  | personRetry (s : WriteOnce) :
      s.begun = true → s.done = false → s.permit = false → s.applied = 0 →
      Step s { s with permit := true }
  /-- D1, `accept_loss`: taken as done, whatever happened. -/
  | acceptLoss (s : WriteOnce) :
      s.begun = true → s.done = false → s.permit = false →
      Step s { s with done := true, lost := true }

/-- The states some sequence of steps reaches from the start. -/
inductive WriteOnce.Reachable : WriteOnce → Prop
  | init : Reachable WriteOnce.init
  | step {s t : WriteOnce} : Reachable s → WriteOnce.Step s t → Reachable t

/-- What holds in every reachable state. -/
def WriteOnce.Inv (s : WriteOnce) : Prop :=
  (s.permit = true → s.applied = 0 ∧ s.begun = true ∧ s.done = false) ∧
  (s.begun = false → s.applied = 0) ∧
  s.applied ≤ 1 ∧
  (s.done = true → s.lost = false → s.applied ≥ 1)

theorem WriteOnce.inv_init : WriteOnce.Inv WriteOnce.init := by
  simp [WriteOnce.Inv, WriteOnce.init]

theorem WriteOnce.inv_step {s t : WriteOnce} (h : WriteOnce.Inv s) (st : WriteOnce.Step s t) :
    WriteOnce.Inv t := by
  obtain ⟨hp, hb, ha, hd⟩ := h
  cases st with
  | start h1 h2 =>
    refine ⟨fun _ => ⟨hb h1, rfl, h2⟩, fun h => by simp at h, by simp [hb h1], ?_⟩
    simp [h2]
  | sendOk h1 _ =>
    have := (hp h1).1
    refine ⟨fun h => by simp at h, fun h => ?_, by simp; omega, fun _ _ => by simp⟩
    have := (hp h1).2.1; simp_all
  | sendLost h1 =>
    exact ⟨fun h => by simp at h, hb, ha, hd⟩
  | sendNoAnswer h1 =>
    have h0 := (hp h1).1
    refine ⟨fun h => by simp at h, fun h => ?_, by simp; omega, fun _ _ => by simp⟩
    have := (hp h1).2.1; simp_all
  | crash =>
    exact ⟨fun h => by simp at h, hb, ha, hd⟩
  | verifyFound h1 h2 h3 h4 =>
    exact ⟨fun h => by simp_all, hb, ha, fun _ _ => h4⟩
  | verifyAbsent h1 h2 h3 h4 =>
    exact ⟨fun _ => ⟨h4, h1, h2⟩, hb, ha, fun h => by simp_all⟩
  | personDone h1 h2 h3 h4 =>
    exact ⟨fun h => by simp_all, hb, ha, fun _ _ => h4⟩
  | personRetry h1 h2 h3 h4 =>
    exact ⟨fun _ => ⟨h4, h1, h2⟩, hb, ha, fun h => by simp_all⟩
  | acceptLoss h1 h2 h3 =>
    exact ⟨fun h => by simp_all, hb, ha, fun _ h => by simp at h⟩

theorem WriteOnce.inv_reachable {s : WriteOnce} (r : WriteOnce.Reachable s) : WriteOnce.Inv s := by
  induction r with
  | init => exact WriteOnce.inv_init
  | step _ st ih => exact WriteOnce.inv_step ih st

/-- **T1** (at most once): whatever the world does, the service applies a
`write once` call at most once. -/
theorem WriteOnce.at_most_once {s : WriteOnce} (r : WriteOnce.Reachable s) : s.applied ≤ 1 :=
  (WriteOnce.inv_reachable r).2.2.1

/-- **T2** (exactly once when finished): finished with `verify` or `pause`
(not taken as done by `accept_loss`), the call was applied exactly once. -/
theorem WriteOnce.exactly_once {s : WriteOnce} (r : WriteOnce.Reachable s)
    (hd : s.done = true) (hl : s.lost = false) : s.applied = 1 := by
  have i := WriteOnce.inv_reachable r
  have := i.2.2.2 hd hl
  have := i.2.2.1
  omega

/-- **T3** (nothing done is redone): once `done` is on disk, no step applies
the call again, and `done` stays. -/
theorem WriteOnce.done_is_final {s t : WriteOnce} (r : WriteOnce.Reachable s)
    (hd : s.done = true) (st : WriteOnce.Step s t) : t.applied = s.applied ∧ t.done = true := by
  have hp : s.permit = false := by
    cases h : s.permit
    · rfl
    · have := ((WriteOnce.inv_reachable r).1 h).2.2; simp_all
  cases st <;> simp_all

/-! ## `write` with an idempotency key -/

/-- One keyed write whose key is a model's answer (a previous step, R1). -/
structure Keyed where
  /-- The model's answer, recorded in the journal and on disk. -/
  disk : Option Nat
  /-- The model's answer, recorded but only in the OS (lost if the machine crashes). -/
  os : Option Nat
  /-- The write's `done`, on disk. -/
  done : Bool
  /-- The keys the service applied the write with (the truth). -/
  keys : List Nat
  deriving DecidableEq, Repr

def Keyed.init : Keyed := { disk := none, os := none, done := false, keys := [] }

/-- The service, honouring keys (H2): a key it already applied changes nothing. -/
def Keyed.apply (keys : List Nat) (k : Nat) : List Nat :=
  if k ∈ keys then keys else k :: keys

inductive Keyed.Step : Keyed → Keyed → Prop
  /-- R1: no answer in the journal, so the model is asked; it may answer
  anything, and the answer is recorded without syncing. -/
  | ask (s : Keyed) (v : Nat) :
      s.disk = none → s.os = none →
      Step s { s with os := some v }
  /-- The journal reaches the disk (an `fsync`, or the OS writing it out). -/
  | sync (s : Keyed) (v : Nat) :
      s.disk = none → s.os = some v →
      Step s { s with disk := some v, os := none }
  /-- R2: the key comes from the journal on disk (the runtime syncs before
  the write); the service applies it, and the answer arrives or not. -/
  | send (s : Keyed) (k : Nat) (answered : Bool) :
      s.disk = some k → s.done = false →
      Step s { s with keys := Keyed.apply s.keys k, done := answered }
  /-- A request lost on the way: nothing applied. -/
  | lost (s : Keyed) : Step s s
  /-- A machine crash: what was only in the OS is lost. -/
  | machineCrash (s : Keyed) : Step s { s with os := none }

inductive Keyed.Reachable : Keyed → Prop
  | init : Reachable Keyed.init
  | step {s t : Keyed} : Reachable s → Keyed.Step s t → Reachable t

def Keyed.Inv (s : Keyed) : Prop :=
  s.keys = [] ∨ ∃ k, s.keys = [k] ∧ s.disk = some k

theorem Keyed.inv_reachable {s : Keyed} (r : Keyed.Reachable s) : Keyed.Inv s := by
  induction r with
  | init => exact Or.inl rfl
  | step _ st ih =>
    cases st with
    | ask v h1 h2 => exact ih
    | sync v h1 h2 =>
      rcases ih with h | ⟨k, hk, hd⟩
      · exact Or.inl h
      · simp_all
    | send k a h1 h2 =>
      right
      rcases ih with h | ⟨k', hk, hd⟩
      · exact ⟨k, by simp [Keyed.apply, h], h1⟩
      · have : k' = k := by simp_all
        subst this
        exact ⟨k', by simp [Keyed.apply, hk], h1⟩
    | lost => exact ih
    | machineCrash => exact ih

/-- **T1 for keyed writes**: even if the machine crashes and the model would
answer differently when asked again, the service applies the write under at
most one key, so at most once. -/
theorem Keyed.at_most_once {s : Keyed} (r : Keyed.Reachable s) : s.keys.length ≤ 1 := by
  rcases Keyed.inv_reachable r with h | ⟨k, hk, _⟩ <;> simp_all

/-! ## What breaks without the syncs

Both are the holes formalizing found in the runtime (§4.1 of the paper),
fixed since. -/

/-- The keyed write as it was: the key could come from an answer still only
in the OS. -/
inductive KeyedNoSync.Step : Keyed → Keyed → Prop
  | ask (s : Keyed) (v : Nat) :
      s.disk = none → s.os = none → Step s { s with os := some v }
  | send (s : Keyed) (k : Nat) (answered : Bool) :
      s.os = some k → s.done = false →
      Step s { s with keys := Keyed.apply s.keys k, done := answered }
  | machineCrash (s : Keyed) : Step s { s with os := none }

/-- The model answers 1; the write goes out with key 1 and its answer is
lost; the machine crashes; on resume the model answers 2; the write goes out
again with key 2. The service applied it twice. -/
theorem KeyedNoSync.duplicates :
    ∃ s1 s2 s3 s4 s5 : Keyed,
      KeyedNoSync.Step Keyed.init s1 ∧ KeyedNoSync.Step s1 s2 ∧ KeyedNoSync.Step s2 s3 ∧
      KeyedNoSync.Step s3 s4 ∧ KeyedNoSync.Step s4 s5 ∧ s5.keys.length = 2 :=
  ⟨_, _, _, _, _,
    KeyedNoSync.Step.ask Keyed.init 1 rfl rfl,
    KeyedNoSync.Step.send _ 1 false rfl rfl,
    KeyedNoSync.Step.machineCrash _,
    KeyedNoSync.Step.ask _ 2 rfl rfl,
    KeyedNoSync.Step.send _ 2 false rfl rfl,
    by simp [Keyed.apply, Keyed.init]⟩

/-- A `write once` whose `begin` a machine crash can lose: after the crash,
the runtime takes the call as never started, and starts it again. -/
inductive WriteOnceNoSync.Step : WriteOnce → WriteOnce → Prop
  | start (s : WriteOnce) :
      s.begun = false → s.done = false → Step s { s with begun := true, permit := true }
  | sendNoAnswer (s : WriteOnce) :
      s.permit = true → Step s { s with applied := s.applied + 1, permit := false }
  /-- The `begin` was only in the OS: the crash loses it. -/
  | machineCrash (s : WriteOnce) : Step s { s with begun := false, permit := false }

theorem WriteOnceNoSync.duplicates :
    ∃ s1 s2 s3 s4 s5 : WriteOnce,
      WriteOnceNoSync.Step WriteOnce.init s1 ∧ WriteOnceNoSync.Step s1 s2 ∧
      WriteOnceNoSync.Step s2 s3 ∧ WriteOnceNoSync.Step s3 s4 ∧
      WriteOnceNoSync.Step s4 s5 ∧ s5.applied = 2 :=
  ⟨_, _, _, _, _,
    WriteOnceNoSync.Step.start _ rfl rfl,
    WriteOnceNoSync.Step.sendNoAnswer _ rfl,
    WriteOnceNoSync.Step.machineCrash _,
    WriteOnceNoSync.Step.start _ rfl rfl,
    WriteOnceNoSync.Step.sendNoAnswer _ rfl,
    rfl⟩

end Calyx
