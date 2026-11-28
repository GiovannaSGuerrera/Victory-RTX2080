# Three isolated CGBJ adaptations

Reference ede4cbb preserved. GPL-3.0-or-later source fork-cgbj f4b48ad7.
No whole-file copy, no THIN/land/upload-ahead/pool imports.

1. Target/depth binding cache: compare host-resource identity AND complete view
record, preserve target/depth tracking, invalidate after internal passes and on
list replay. Opt-in VICTORY_BIND_CACHE=1. Tests descriptor changes/nulls/reset.
Build checkpoint 1 with other new features absent.
2. Indirect budget recovery: an oversized rejected record costs zero rather
than exhausting remaining budget; exact fit still consumes full budget. Opt-in
VICTORY_INDIRECT_RECOVER=1; explicit HLSL define. Test actual GPU clamp on WARP,
including oversized->small, exact-fit, count buffers and old-path compatibility.
Build checkpoint 2.
3. Configurable pipeline workers and priority: adapt fork controls while retaining
local scheduler and old defaults. VICTORY_PIPELINE_WORKERS explicit bounded
count; VICTORY_PIPELINE_LOWEST=1 explicit low priority. Test bounds/defaults and
existing queue tests; build final candidate. Document no steady-state FPS claim.

All new options independently selectable; no automatic baseline replacement.
Run relevant tests, policy and diff checks. Preserve previous experimental exes.
Record hashes/results in PROGRESS.md and technical memory. No publish/commit
unless requested. AI assistance: OpenAI Codex GPT-6.
