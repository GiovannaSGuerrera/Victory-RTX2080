# Instruction EXEC guards for descriptor tracing

Reference: ede4cbb, preserved executable victory-buffer-pass.exe.

1. Reproduce the synthetic fork-cgbj join-access test against the old tracer.
2. Adapt only instruction site tracking and EXEC guard assumptions in descriptor/live.
3. Gate with VICTORY_RESOURCE_INSTRUCTION_GUARDS=1, disabled by default.
4. Include the switch in shader-cache identity; preserve previous cache data.
5. Test disabled/enabled explicitly, run shader tests, build a separate Release candidate.
6. Run Series S at the existing 1280x720 profile with other experiments disabled.
7. Separately check driver refusal of 2da4beb386509b77, visual artifacts and frame times.

Source: brenofrancoa/fork-cgbj f4b48ad766ddbec1a1867022cd42f7645b0dd55e,
GPL-3.0-or-later. No whole-file port or hardware-specific profile copied.

Confirmed: before the fix the synthetic access fails at offset 56 with
"a descriptor neither loaded nor built". With the fix enabled it resolves
a constant sampler; disabled it still returns an error as expected.
Runtime correctness and FPS benefit remain unproven.
