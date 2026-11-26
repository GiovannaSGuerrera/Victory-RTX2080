# Credits

Local buffer-pass experiment (2026-11-26): OpenAI Codex (GPT-6), at the
direction of GiovannaSGuerrera. The opt-in change builds on this tree's existing
heap-pass self-mark suppression and incremental planner; it preserves per-pass
orphan retries and adds local CPU/GPU sampling. Existing source credits below
remain applicable.

This tree is published as a single source snapshot, so the history that
credited each change commit by commit is not part of it. This file carries
that credit instead. People are named by their public GitHub login, or by the
name on their commit where no login is known. Commit hashes are those of the
source project (the commit a change was taken or ported from), so they can be
looked up there.

Every project listed below is licensed **GPL-3.0-or-later** (as its `LICENSE`
and `Cargo.toml` state), the same licence as this tree. No code from XCL
(GPL-2.0-only) is in this tree: an instruction decoder once ported from it was
removed, and the one change below that was inspired by XCL was written anew.

## Base

- **[Marvisak/VIctory](https://github.com/Marvisak/VIctory)** by Marvisak and
  its contributors: the loader, runtime services, Direct3D 12 back end and
  shader recompiler on which everything here rests.
- **[fxceless/victory-fork](https://github.com/fxceless/victory-fork)** by
  fxceless and its contributors, up to its commit `c96b4f2b`: the fork this
  tree starts from. Both projects' own histories credit their contributors.

## Marvisak/VIctory (upstream)

| Contributor | Change | Source |
|---|---|---|
| Tex | `clwb_loop` shortcut guarded against u64 overflow and wraparound | upstream `274f6988` |
| Maledetto (Makto94) | `clzero` loops handled in one exception; `rdpru` | upstream `0de9efce`, based on #34 by ka1qux (`faa0a9a`) |
| moe-miau | a stray or unreadable save version folder never costs the save | VIctory #1729 (`b8dbcba2`) |
| adamMasMusic | an old save version another process holds open no longer fails the save | VIctory PR #1838 (`5b40efda`) |
| desp | depth twins share their scratch buffers (`VICTORY_DEPTH_SCRATCH_SHARED`), ported | VIctory PR 2162 (`542154a9`) |
| Marvis | translator-hash build script used by the shader cache key | upstream `c47b529` (co-author of fork PR 544) |
| Aydan (aycow) | disjoint shadow refresh ranges kept (adapted in fork PR 591) | VIctory #1818; reproduction by pablogonz12 in VIctory #185 |
| TG0000 | per-name guard for pipeline library misses (adapted in fork PR 503) | VIctory #1000 (`f821cb9`) |
| tightfall | set of aliased blocks (part of the view guard, via myokex) | VIctory #1122 |
| Fuzeofficial | dword 0 of root constants is no address base in the translator's tracer | VIctory #2154 (`88daba42`) |
| desp | a program's constant table starts after its instructions (less NVIDIA video memory) | VIctory #2143 (`e04c0781`) |
| Marvis | table constants read in place, through a raw view of their host copy (that part only, ported; `VICTORY_TABLE_CONSTANTS`) | upstream `683ad7a9` |
| tjthebigbearcat-hub | mesh primitive vertex indices are 9 bits (the index mask; its tests not taken), via fork PR 1112 | upstream `c0228d9` |

## fxceless/victory-fork (pull requests and main)

| Contributor | Change | Source |
|---|---|---|
| fxceless | packed textures stay in the upload queue; refresh of textures on a full card | fork main `310c6eb6` |
| fxceless | `VICTORY_COPY_PLAIN` in-place copies (ported) | fork main `a2537066`, `0b0074e0` |
| fxceless | `VICTORY_ASYNC_SUBMIT` submit thread; `VICTORY_QUEUE2` copy-queue fetches with refetch (ported) | fork main `f85101b6` |
| fxceless | scattered indirect records clamped once per list (`VICTORY_INDIRECT_GATHER`, ported) | fork main `465b94f5` |
| sagouindupm | Series S: second present plane drawn over the first, upscale dispatch as a stretch, stretch bounds and tests, serialized root signatures | fork PR 188 (`8adb7aaa`, `19a0240b`, `82231c33`, `c022e4d1`; stretch edge `3ed3b19a`) |
| Filip722 | read-back test of the Series S stretch and overlay | fork PR 188 (`76687ee3`) |
| armin | 16-bit float RGBA stands in for R9G9B9E5 render targets; its log line | fork PR 188 (`7dc19f21`, `9ba864a8`, from fork PR 140) |
| armin | DirectStorage page-destination writes told to the GPU side | fork PR 571 (`ec57c536`) |
| sagouindupm | a tap is kept until the title has read it | fork PR 640 (`f0fecd22`) |
| triippywhale | translator: unused descriptors' sizes, paired f16 interpolation halves, 16-bit wave sums | fork PR 203 (`0e49a91c`) |
| Vyacheslav Venglovsky | indirect scratch buffer barriers, clamp budget per thread group and in card memory, benchmarks; `VICTORY_INDIRECT_WINDOW_CHECK` | fork PR 220 (`cc11cccc` .. `7e239bfb`, `f8733949`) |
| Vyacheslav Venglovsky | faster bindless texture queue selection | fork PR 221 (`5c7e9c99`) |
| YeokiNoewChi | refused DirectStorage reads into guarded memory copied through a buffer | fork PR 426 (`fc070b41`) |
| iurysf | table constant buffers that flights write filled on the card | fork PR 462 (`b39d4aa9`) |
| LeonardoS11 | `VICTORY_COPY_COMPARE` (only changed 4 KiB pieces refreshed); `VICTORY_RT_OFF` | fork PR 465 (`1b55ade8`, `dde63a7e`) |
| NickJBc | wide card hash (`VICTORY_CARD_HASH_WIDE`) | fork PR 493 (`ccdf4c4b`) |
| DoubleD17 | pipeline library misses serialized by name | fork PR 503 (`a3f997be`) |
| Adrito (adritoyalser) | bound view heap checked before the early texture pass claims the frame | fork PR 506 (`0845d6d4`) |
| LeonardoS11 | heap buffer pass looks only at marked slots | fork PR 523 (`37272aea`) |
| justrealyme | shader cache keyed by the translator; `translate --cache` warm-up | fork PR 544 (`bd00054b`) |
| NickJBc | texture recheck fingerprint reads every mip of every slice | fork PR 559 (`6f1fc6b5`) |
| AscilCH | dropped heaps evicted from the memory heap table | fork PR 589 (`3f95baf9`) |
| DoubleD17 | disjoint shadow refresh ranges preserved | fork PR 591 (`0d22b380`) |
| FrancescoGelosoEterea | a lane-branch region that only raises an LDS flag runs as two host waves | fork PR 592 (`8bc48bf7`) |
| orazal12 | translator: redundant heap clamps skipped, stale DOUBLES flag, `v_permlane`, wave32 f64 sum | fork PR 598 (`e8981f30`, #505) |
| laptopnotebook | a remap under an uploaded texture uploads it again | fork PR 606 (`a4537444`, #574) |
| DiogoPCastelos | textures spill into the memory the system shares with the card (`VICTORY_TEXTURE_SPILL`, off by default; written after that PR) | fork PR 601 (`b70115e1`) |
| ofc0urse | `VICTORY_TEXTURE_DROP_MIPS` | fork PR 610 (`5971e08b`) |
| C-GDev | `VICTORY_SKIP_BINK` (idea from XCL PR #1053, code written anew) | fork PR 613 (`75fe122c`) |
| laptopnotebook | RGBA8 shader views keep their sRGB or linear format | fork PR 644 (`8407e3d0`, #578) |
| Jonirulah | card copies' pieces fetched inside their own flights (`VICTORY_CARD_FETCH_INFLIGHT`), and its staged form | fork PR 648 (`ca2513c0`, `4d284865`) |
| Pin0d1 | prefetched indirect arguments invalidated on recorded writes (ported) | fork PR 663 (`845fc43e`) |
| FJBP-code | relay card fetches on a dedicated copy queue | fork PR 665 (`15579433`) |
| Jonirulah | kernel write-watch asking skipped for known guarded view memory | fork PR 670 (`05d37755`) |
| AscilCH | submissions with duplicate host lists split before executing | fork PR 675 (`92ccda22`) |
| AscilCH | full-card texture queue starvation (parts 1 and 2, ported) | fork PR 676 (`f130ba1a`) |
| Claudinhomerq | clean watch ranges merged, write-watch skipped on guarded views, `VICTORY_D3D12_DRED=0` respected | fork PR 685 (`29f3943d`) |
| abilliontokens | missing textures as checkerboards (`VICTORY_DEBUG_NULL_VIEWS`, one hunk) | fork PR 687 (`e54d6978`) |
| traeb0n | textures written behind their guards hashed at every check | fork PR 701 (`802684eb`) |
| AscilCH | `Counting::realloc` race and monotonic pipeline wait budget | fork PR 709 (`037ec18d`) |
| LeonardoS11 | card copies keep room from textures picked by index (`VICTORY_CARD_RESERVE`) | fork PR 723 (`cb124b60`) |
| marquezini | texture staging buffers from a pool | fork PR 781 (`67efc51b`) |
| Chipsrain, iGoldenNinja | census of card copies; write-combining memory left on the card (`VICTORY_SKIP_WC_BACK`), hand port | fork PR 868 (`7e64279d`, `fef95924`, `b57479c4`, `f9c43fab`) |
| douline | constant buffer views at most 64 KiB (ported) | fork PR 911 (`10875383`) |
| tippythetaidum | halves moved through 16-bit buffer views keep their bits, and three follow-ups | fork PR 927 (`5dc9e63f`, `befaf3b5`, `b6bf6516`, `b9c37813`) |
| author recorded as "VIctory Contributor" | a shadow stays alive until its submission's snapshot jobs finish (ported) | fork PR 936 (`b212918c`, `c00ac405`) |
| HailLucifer666 | an in-place heap is held until the submissions that could read it finish (ported) | fork PR 943 (`2185fe57`) |
| Bruno Magalhães (brunofgmag) | tiled cascade shadow programs translate exactly on 32-lane hosts | fork PR 1002 (`eaeb8f17`) |
| SpaghettiMaster545 | the GPU side is told of an unmapped view after the unmap | fork PR 1005 (`6f11f27c`) |
| author unknown | textures with mip data kept mapped between uncommitted ends | fork PR 1023 (`4c29c4db`) |
| UwUCammie | fault-guarded in-place write-back for `VICTORY_COPY_PLAIN` | fork PR 1032 (`d919676c`) |
| Key | DirectStorage worker pool sized to physical cores | fork PR 1044 (`0bba7624`) |
| UwUCammie | RwLock read fast path for shadow buffers (that part only) | fork PR 1068 (`ee7a5666`) |
| m335671 | a least amount of indirect light while rays do not run (`VICTORY_AMBIENT`), ported from Belobug/VIctory-Belobug-AMD | fork PR 1093 (`e94e4146`) |
| Azemed | immediate texture flush before a decoration bake (`VICTORY_BAKE_FLUSH`, that part only, ported) | fork PR 1097 (`a8bcf21d`) |
| KyleS98 | gathered four translator fixes by DexNom and tjthebigbearcat-hub (credited in their own rows) | fork PR 1112 (`4bc83a69`, `20be56ab`, `ade7aa10`, `bed3396d`) |
| Alexis | a texture refresh lets title memory move again once its bytes are copied (`VICTORY_TEXTURE_SHORT_HOLD`, off; ported) | fork PR 1078 (`4f83d40d`) |
| beamdotdev | descriptor heap locks in pieces (`VICTORY_HEAP_LOCKS`); landed lists reset on a thread of their own (`VICTORY_LAND_ASIDE`, off) | fork PR 1146 (`6d1fe2f6`) |

## Geckomacabre/vx-layer

| Contributor | Change | Source |
|---|---|---|
| Geckomacabre | ranges noted for the texture pass joined, not dropped, past the limit | `3535d0a0` |
| Geckomacabre | `VICTORY_TEXTURE_HOT` | `dca26157` |
| Geckomacabre | small pipelines first, big compute programs on fewer workers | `f4b41161` |
| Geckomacabre | `VICTORY_REMAP_BATCH` | `d5106b6e` |
| Geckomacabre | DirectStorage reads of one file side by side, each through its own handle | `9c25843e` |
| Geckomacabre | `VICTORY_WAVE_WIDE_ONLY` | `aebe7823` |
| Geckomacabre | depth planes written by shaders reach the plane before the next draw | `00633243` |
| Geckomacabre | shader-written textures copied back into title memory (`VICTORY_TEXTURE_BACK`) | `eee5667d` |
| Geckomacabre | an address destination's DirectStorage write told again once landed (that hunk, ported) | `d4be2b9f` |
| uiliannn | copy-back ranges released after the landing is published | `822e76ee`, `691f747b` |

## RecompLive/victory-fork

| Contributor | Change | Source |
|---|---|---|
| RecompLive | a fresh descriptor table reserves only its used slots (adapted) | `6b44e423` |
| RecompLive | tracked views stay dirty while any alias cannot be guarded (adapted) | `bf7a5e74` (victory #2455) |

## triippywhale/victory-fork

| Contributor | Change | Source |
|---|---|---|
| triippywhale | the views' table shared by readers (RwLock), port of PalmVIDev #136 | `b8f8e8a6` |
| dharmon96 | ray-tracing walkers get empty structures while the builder is off, port of PalmVIDev #223 | `f54cb72f` |
| Adrito (adritoyalser) | placed-texture filler words ignored in `same_placed_texture` and `Spans::bytes` | `96800256` |
| Manuel Pérez | `SHADER_HEAPS` held across the descriptor mirror; query offsets checked | `08fa0528` |
| Last Thylacine | reads in place and remaps hold only the bytes they touch (`VICTORY_MAPPING_RANGES`) | `c79c751c` |
| EricPlayZ | fills of upload memory read together (`VICTORY_JOIN_FILLS`, hand port), port of PalmVIDev #229 and #245 | `e2328bed` |
| commits authored "Claude" in that fork | `VICTORY_HEAP_PASS_UNMARKED`; `VICTORY_WRITE_ASIDE` and `VICTORY_ALIAS_LAZY` (hand port) | `64e58c46`; `2b64a9de`, `b9fe06d6`, `7e88d165` |
| triippywhale/victory-fork | title copies go through the kernel when a remap races them (adapted) | `b0286796` |

PalmVIDev's original work (#136, #223, #229, #245) reached this tree through
triippywhale's fork; PalmVIDev's own repository no longer resolves.

## dexnom/VIctory

| Contributor | Change | Source |
|---|---|---|
| Adika713 | the per-command draw state lock removed | dexnom PR 27 (`da49ccc4`) |
| Adika713 | the deferred-work lock removed from host lists | dexnom PR 29 (`5aa5e0c3`) |
| DexNom | a wave64 lane mask restored from two 32-bit scalar copies is the mask again (via fork PR 1112) | `76749ce` |
| DexNom | a uniform branch choosing between two immediate samplers samples with both (the translator part, via fork PR 1112) | `ddb7332` |
| DexNom | a cube sample with explicit gradients takes log2 once and keeps its minimum level (that hunk, via fork PR 1112) | `921d8d3` |

## Other projects

| Contributor | Change | Source |
|---|---|---|
| Adam | contiguous free descriptors found across the search cursor; slots of a rejected bind given back at once | Xeltax/VIcky `28c94c11`, `ab88cd20` |
| Corgan | `VICTORY_WAVE_LOOSE_LDS` (the loose-LDS part only) | Corgansm/victory-corgi `53e7682d` |
| Belobug | the ambient pass (`ambient.rs`), via fork PR 1093 | Belobug/VIctory-Belobug-AMD |
| Filip722 | dropped mip bytes skipped when refreshing 2D textures; retained levels read back on reduced textures | Filip722/VIctory `31694102`, `81e1f7a8` |
| myokex | a write through one view of a page is seen at its other views; code of victory-fork #115 by Spexivus, with joroyt's correction and tightfall's set of aliased blocks (VIctory #1122) | myokex/victory-amd `c6dc4d8c` |
| ryotdev | successful input readings counted from the source of a tap | ryotdev's fork, `317d7a48` |
| pgm1207 | no blending on integer render targets in host pipelines | pablogonz12's repository, `169efcb8` |

## Written for this tree

The remaining changes were written for this tree by CuteBooty with AI
assistance (Claude Code; OpenAI Codex where the change was written or ported
by Codex). Among them: the hand ports and adaptations named above; the
decoration bake flush and loading bake budget; the present settle for
flights that copy results back (`VICTORY_BACK_SETTLE`,
`VICTORY_TEXTURE_BACK_SETTLE`); safer texture copy-back landing; the dynamic
descriptor ring work (single slots from the top, word search, early release,
packing, sharing and reclaiming, `VICTORY_DYNAMIC_SLOTS`); small textures on
a full card (`VICTORY_SMALL_TEXTURE_KB`); `VICTORY_ASYNC_SUBMIT_SAFE`; queued
taps; save logging; the removal of the XCL-derived instruction decoder; and
switches that turn merged changes off; the later dynamic descriptor ring
work (unbound tables skipped, upload-table slices reused, landed flights
swept continuously, free runs joined; `VICTORY_DYNAMIC_*`); read-back
tracking (`VICTORY_FRAME_FEEDBACK`); the port of upstream's table constants
read in place, and the follow-up to `88daba42` for pointers in root
constants; and four defect fixes found by an outside review (a later list of
a submission waits for an earlier list's read-back; a boxed read-back writes
back only the box; a fresh table holding constants is not reused across a
title write; the present keeps the `VICTORY_BACK_SETTLE` policy).

Several contributors' own commits also say they were written with AI
assistance (Claude Code, OpenAI Codex); commits in triippywhale's fork carry
the author name "Claude". Commits co-authored by "OpenBot" (fork PRs 462 and
685) are credited above to their human authors.

### Three local adaptations from CGBJ

Target/depth binding reuse, indirect oversized-record budget recovery and
pipeline worker controls are adapted from brenofrancoa/fork-cgbj revision
f4b48ad766ddbec1a1867022cd42f7645b0dd55e (GPL-3.0-or-later), retaining local
scheduling defaults and opt-in switches. Source: draw.rs, descriptor.rs,
indirect.rs and pipeline/make.rs. Local tests and integration: AI assistance
by OpenAI Codex, GPT-6. Upstream authorship remains credited by the fork history.
