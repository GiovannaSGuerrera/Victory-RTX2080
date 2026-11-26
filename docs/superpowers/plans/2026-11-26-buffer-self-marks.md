# Buffer self-marks implementation plan

Goal: reduce repeated descriptor work without delaying orphan retries; measure CPU threads and GPU usage in a separate Release candidate.

Architecture: opt-in VICTORY_HEAP_PASS_NO_SELF_MARK=1 suppresses marks made by the buffer pass itself. Its planner retries orphans every pass, preserving the reference retry cadence. External changes, record comparisons, freed entries and periodic full passes remain active.

Tech stack: Rust, Cargo Release, PowerShell, Windows performance counters, NVIDIA SMI.

Spec: user request to improve FPS from startup and measure CPU/GPU utilization; root PROGRESS.md contains the measured reference.

- [x] Add opt-in planner setting and synthetic coverage for orphan retries, unchanged valid views and external changes.
- [x] Connect mark suppression to the same setting; leave defaults intact.
- [x] Build Release, preserve reference binary and copy candidate separately.
- [x] Add sampler for normalized process CPU, thread CPU, logical processors and GPU; validate sampler on a synthetic local process.
- [ ] Run candidate with mapping ranges plus this one new optimization and compare logs/visuals. Do not infer gains from build success.

Review focus: unchanged defaults; orphan becomes available next pass; resource freed without record change; direct record writes; sampler overhead and per-thread CPU denominator (one logical processor =100%).

Candidate launched and real third-person character/environment observed. Same-route performance comparison remains outstanding; no causal improvement claimed.
