# Local CPU / buffer-pass experiment

The reference binary remains `target/release/victory.exe`. The candidate is
`target/release/victory-buffer-pass.exe`; neither saves nor cache are replaced.

Run through the existing entry point:

```powershell
& J:\GTA6\Iniciar-VIctory-720p.ps1 -ExperimentalBufferPass
```

This uses the preceding mapping-ranges experiment as its reference:
`VICTORY_MAPPING_RANGES=1`, default bake budget, and only one additional
optimization: `VICTORY_HEAP_PASS_NO_SELF_MARK=1`. The new switch avoids marking
descriptor slots made by the buffer pass itself. Orphan views remain eligible
every pass; external descriptor writes, direct record changes, freed entries and
periodic full passes retain their invalidation paths. Defaults remain unchanged.

The candidate launcher starts a read-only usage sampler every five seconds and
records `usage.jsonl` alongside the runtime logs. It stops when the tracked
process exits, including a check against PID reuse. It never terminates the game.

`process_cpu_total_pct` is normalized across all logical processors. Each
`busiest_threads[].one_logical_cpu_pct` uses 100% for one occupied logical CPU.
`system_logical_cpu_pct` covers all applications. NVIDIA utilization, memory
controller, VRAM, temperature and power readings are device-wide, not exclusive
to the game. Sampling cost and missing counters are recorded explicitly.

Compare the same scene and route at the same resolution, warmed cache and
driver. Separate startup, loading, cutscene and gameplay. Log windows are not
individual-frame percentiles. Verify image correctness and long pauses before
promoting the experiment; utilization alone does not identify a bottleneck.

## Observed checkpoint (2026-11-26)

Release build succeeded; five buffer-planner tests passed. The local asset-policy
check passed. Candidate SHA256:
`C609C7D3411D9B929A97A87B9286401B5385BAD31A849FA426CD5E0A83A887EB`.

The game displayed Lucia in third person by an interior door, with character,
wall textures and lighting visible; window counter approximately 6 FPS. Movement
was not tested by the agent. In an earlier partial sample of 101 scene log
windows, median frame time was 196.2 ms, descriptor checks 87.2 ms, copies 75.6 ms.
Nearby usage readings had median process CPU 62.75% and device-wide GPU 52%.
There was still a 3747.4 ms frame-time window. Scene/route differs from reference:
these figures do not establish a causal gain or complete visual correctness.

Runtime logs, usage recordings, game data and binaries remain outside Git.
