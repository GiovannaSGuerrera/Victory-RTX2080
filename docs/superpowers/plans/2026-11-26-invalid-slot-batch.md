# Invalid-slot batch implementation plan

Goal: reduce repeated acquisition of HostHeap's made lock for invalid buffer
views, without skipping cleanup or clearing a concurrently published view.

Architecture: opt-in VICTORY_HEAP_INVALID_BATCH=1 queues invalid lookups only
inside the buffer-preparation pass. Before the pass ends, take the same lock
once and revalidate every queued record. Clear still-invalid slots using the
existing cleanup; schedule newly valid slots for the planner's next pass.

Tech stack: Rust, Cargo Release, existing PowerShell launcher and GPU timings.
Spec/evidence: root PROGRESS.md; mirror-detail-final.json of session070539.

- [x] Add a revalidation helper with tests for publication and later release.
- [x] Integrate queueing only in the buffer pass; preserve external mirror calls.
- [x] Add explicit candidate switch and invalid_flush_ms to sampled timings.
- [ ] Verify focused tests, Release build, script parsers and asset policy.
- [ ] Compare full buffer-pass time and contention on other threads, including
      images, resource invalidations and long pauses in the same game scene.

Constraints: flag defaults off; published/reference binaries remain intact;
never compare mirror_ms alone after cleanup moved to invalid_flush_ms. Batch
lock hold time may affect other threads; no FPS gain claimed before validation.
