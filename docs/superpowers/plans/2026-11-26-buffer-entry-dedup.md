# Deduplicate buffer invalidation entry lists

Goal: avoid repeated (entry, slot) registrations from orphan retries, without
changing resource reads, refresh intervals, descriptor creation or synchronization.
Reference: ede4cbb. Shader fix, invalid batching and parallel refresh disabled.

1. Reproduce duplicate growth using synthetic orphan retries.
2. Add opt-in VICTORY_HEAP_ENTRY_DEDUP=1, disabled by default.
3. Keep a set of registered pairs; remove registrations when consuming an entry
   or clearing the planner. Preserve existing generation/record checks.
4. Test remapping, shared entries, freeing/reusing entries, periodic resets,
   orphan-to-valid transitions and equivalence against the old planner.
5. Measure bookkeeping on synthetic repeated retries; build separate Release.
6. Compare real frame times at the same camera before adopting.

No FPS improvement is assumed from a synthetic speedup. Existing profiles place
plan/forget/track work in the tens of milliseconds, separately from copies.
