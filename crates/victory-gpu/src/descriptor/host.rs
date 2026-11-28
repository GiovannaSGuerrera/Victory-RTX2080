//! Host descriptor heaps behind the title's shader-visible heaps (decision
//! 0007, step 3). Translated shaders index the heaps directly at the
//! title's descriptor positions, so each host heap keeps the title heap's
//! order: slot `i` is made from what the title wrote at slot `i`, when it
//! writes it (view creation and descriptor copies) and again when a draw
//! reads it through a table and finds the slot changed.

use std::sync::Mutex;

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use super::{VIEW_TAG, View, view_at};
use crate::buffer_profile::{mirror_measure, mirror_path};
use crate::host::host;

/// The slots of one piece of [`HostHeap::kept`].
const KEPT_PIECE: u32 = 1024;

/// Number of shards for the `made` mutex. Power of two for fast modulo.
/// 64 shards: enough to spread contention across threads (2026-11-27: 21ms
/// median wait on single mutex).
const MADE_SHARDS: usize = 64;

/// Number of shards for `placed`, `textures`, `buffers` mutexes.
/// 16 shards: these have lower contention than `made` (2026-11-28: 9.6ms
/// median wait on `note_buffer`).
const NOTE_SHARDS: usize = 16;

/// Whether a slot made from the same record already is found without the
/// heap's lock ([`HostHeap::made_now`]). On by default;
/// `VICTORY_HEAP_LOCKS=0`: under the lock, as before.
fn heap_locks() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_HEAP_LOCKS").is_none_or(|v| v != "0"))
}

pub(crate) struct HostHeap {
    pub(crate) heap: ID3D12DescriptorHeap,
    cpu: usize,
    step: u32,
    /// Slots mirroring the title's; view heaps have `dynamic` more.
    count: u32,
    /// Dynamic slots past `count`: [`DYNAMIC`] in view heaps, none in
    /// sampler heaps. (Upstream falls back to 2^16 on a capped host heap;
    /// ours keeps 2^18 there: 2^16 and 2^17 both ran out in this title.)
    dynamic: u32,
    sampler: bool,
    /// The record (view index and generation) each slot was last made from.
    /// Sharded into `MADE_SHARDS` mutexes to reduce contention.
    made: Vec<Mutex<Vec<u64>>>,
/// The same, readable without the lock: written under it wherever
/// `made` is. A slot whose record has not changed is the common case
/// (every table entry of every draw asks), and the recording threads
/// stood in line for the lock to find that out (a fifth of their time
/// in list calls waiting here and in `kept`, profile of 2026-11-25 on
/// a six-thread processor). `VICTORY_HEAP_LOCKS=0`: the lock always.
made_now: Vec<std::sync::atomic::AtomicU64>,
/// Cached `View` for slots already made: avoids `VIEWS` RwLock + `view_at()`
/// when `made_now` matches. Only for slots that have been made (key != 0).
/// Written under `made` lock; read without lock on fast path (data race
/// benign: stale cache just misses fast path).
made_view: Mutex<Vec<Option<super::View>>>,
    /// Cache for slots known to be invalid (tag != VIEW_TAG or view_at returned None).
    /// Avoids repeated VIEWS RwLock lookups and note_buffer calls for known-invalid slots.
    /// Bit 0 = invalid, Bit 1 = checked this frame. Cleared when slot is made valid.
    invalid_cache: Vec<std::sync::atomic::AtomicU8>,
    /// The host resource each slot views, held while the slot does: shaders
    /// index heaps directly, so a slot the title has not rewritten since it
    /// released the resource may still be read, and a freed host resource
    /// read there is a GPU page fault (the console reads stale memory).
    /// In pieces of [`KEPT_PIECE`] slots, each behind its own lock: the
    /// recording threads make their tables' slots at the same time.
    kept: Vec<Mutex<Vec<Option<ID3D12Resource>>>>,
    /// Which dynamic slots are taken (by lists not yet reset), and where
    /// the searches for free ones start ([`Ring`]).
    next: Mutex<Ring>,
    /// Single dynamic slots are taken from the top of the ring down, longer
    /// tables from the bottom up ([`dynamic_split`]).
    split: bool,
    /// How many dynamic slots are free (changed under `next`'s lock): with
    /// fewer than a table needs, the search is not made. Once the world
    /// had taken them all, every later table of the frame searched every
    /// slot before it was refused.
    free: std::sync::atomic::AtomicU32,
    /// The shortest table that found no run of free slots since slots were
    /// last given back into a run at least that long (changed under
    /// `next`'s lock; `u32::MAX`: none).
    /// Until something is given back the free runs only shrink, so a table
    /// of that length or longer is refused without the search. Enough
    /// slots were free, but in pieces, and every such table walked the
    /// whole ring before it was refused: 0.33 ms each with 131,072 slots,
    /// 1.0 s of the recording threads in a frame that refused 3,143 (run
    /// 45), under the lock all of them need.
    too_long: std::sync::atomic::AtomicU32,
    /// View heaps: the bytes per element of the buffer view at each title
    /// slot, for shaders that pick a buffer by an index they compute
    /// (`victory_shader::emit::SLOT_STRIDES`): an upload buffer of
    /// `SLOT_STRIDES_COUNT` dwords, its GPU address and its mapping.
    strides: Option<(ID3D12Resource, u64, usize)>,
    /// Title slots holding buffer views. Such a view gets its host slot
    /// (and its memory a host copy) only when something reads it: a draw
    /// through a table, or [`HostHeap::buffer_slots`] for shaders that pick
    /// buffers from the whole heap.
    /// Sharded into `NOTE_SHARDS` mutexes.
    buffers: Vec<Mutex<std::collections::BTreeSet<u32>>>,
    /// Of those and of the slots that left the set: the ones written since
    /// [`HostHeap::buffer_changes`] last took them.
    changed: Mutex<std::collections::HashSet<u32>>,
    /// Title slots holding a view of a texture the title may fill itself (a
    /// placed texture, a texture object), with the frame from which they
    /// are due: such a texture is uploaded when a draw reads it through a
    /// table; one a shader picks by a computed index is brought up to date
    /// from here ([`HostHeap::texture_slots_due`]).
    /// Sharded into `NOTE_SHARDS` mutexes.
    textures: Vec<Mutex<TextureQueue>>,
    /// Placed texture spans for stale tracking.
    /// Sharded into `NOTE_SHARDS` mutexes.
    placed: Vec<Mutex<Spans>>,
    /// Per title slot, the frame (plus one; 0: never) in which a draw last
    /// read it through a table, or made a table it lies in (with
    /// [`TABLED_READ`] set if the draw read it). The draw that
    /// reads such a slot brings its texture up to date itself and the slot
    /// then leaves `textures`; a write of a slot that was a table's within
    /// the last [`TABLED_FRAMES`] frames is due only [`TABLED_GRACE`]
    /// frames later, by when its draw has read it. A title that builds a
    /// table per draw writes thousands of texture views a frame into slots
    /// only tables read (the title: some 2,500 a frame in its
    /// opening cutscene, a few hundred textures over and over), and every
    /// one of them stood in the queue of textures picked by index, ahead
    /// of or behind the ones shaders do pick by index.
    tabled: Vec<std::sync::atomic::AtomicU32>,
    }

#[cfg(feature = "diagnostics")]
impl Drop for HostHeap {
    fn drop(&mut self) {
        crate::copystat::dynamic_returned(self.dynamic.saturating_sub(*self.free.get_mut()));
    }
}

/// Title memory by the placed texture views that describe it: per span of
/// memory (its start, its end at full size) the title slots holding a
/// view of a texture that takes exactly that span. A console title need
/// not clear a descriptor when it streams a texture out, so a newer view
/// that cannot be there together with an older one is all that says the
/// older one is stale ([`Rule`] says when that is). Views of one span with
/// different descriptions (the formats of one family, say) are one
/// texture's.
///
/// Spans may overlap: a texture's smallest levels lie at the start of its
/// memory, and a title that holds only those puts the next texture where
/// the larger ones would be (the title: three 512x256 textures
/// of 0x2c000 bytes each whose starts are 0x2000 and 0x4000 apart, all
/// three read by the same frame's draws, run 188). What is there of a
/// texture ends where the next one starts.
/// No host heap is needed, so it is tested without one.
#[derive(Default)]
pub(crate) struct Spans {
    /// When a newer view makes an older one stale.
    rule: Rule,
    by_start: std::collections::BTreeMap<u64, (u64, std::collections::BTreeSet<u32>)>,
    by_slot: std::collections::HashMap<u32, u64>,
    /// The bytes a placed texture takes, by resource description: worked
    /// out once for each (a title writes the views of a few thousand
    /// descriptions, thousands of times a frame).
    sizes: std::collections::HashMap<[u32; 14], Option<u64>>,
}

/// When a view the title writes makes an older view of other memory stale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Rule {
    /// When it starts where the older one starts: at the same address
    /// with another length, or within the older one's first [`HEAD`]
    /// bytes (or the older within its own). No two textures share the
    /// block of their smallest levels. A view that only reaches into the
    /// address range of an older texture's larger levels says nothing.
    #[default]
    Start,
    /// Whenever their spans at full size overlap (the rule until run
    /// 188, where it made null the slots of textures packed as above:
    /// each view written took its neighbours out of the queue and off the
    /// card, for good). `VICTORY_STALE_TEXTURES=overlap`.
    Overlap,
    /// Never; a view that would be stale under [`Rule::Overlap`] is left
    /// out of the spans instead. `VICTORY_STALE_TEXTURES=0`, as before.
    Never,
}

/// The first bytes of a placed texture that are its own under
/// [`Rule::Start`]: the 4 KiB block its smallest levels share. (Of 129
/// pairs of overlapping neighbours in the listings of runs 188 and 189,
/// none starts closer than 0x2000 to the one before it.)
const HEAD: u64 = 4096;

impl Spans {
    /// The bytes of title memory a placed texture with the resource
    /// description `desc` takes (`crate::resource::placed_bytes`).
    fn bytes(&mut self, desc: &[u32; 14]) -> Option<u64> {
        let key = crate::resource::placed_desc_key(desc);
        if let Some(&known) = self.sizes.get(&key) {
            return known;
        }
        let bytes = crate::resource::placed_bytes(&key);
        self.sizes.insert(key, bytes);
        bytes
    }

    /// The starts of the spans that overlap `[start, end)`, each taken to
    /// end where the next one starts: the last span that starts before
    /// `start`, if it reaches past it at full size, and every span that
    /// starts inside.
    fn overlapping(&self, start: u64, end: u64) -> Vec<u64> {
        let before = self
            .by_start
            .range(..start)
            .next_back()
            .filter(|(_, span)| span.0 > start)
            .map(|(at, _)| *at);
        before.into_iter().chain(self.by_start.range(start..end).map(|(at, _)| *at)).collect()
    }

    /// Slot `index` holds a view of a texture in `[start, end)`. `newer`:
    /// the title wrote it now, so the slots whose views cannot be there
    /// with it ([`Rule`]) are stale: they are returned, and forgotten.
    /// Otherwise (a draw made the slot from what it held), and under
    /// [`Rule::Never`], a view that clashes with other spans is left out.
    fn note(&mut self, index: u32, start: u64, end: u64, newer: bool) -> Vec<u32> {
        let same = |span: Option<&(u64, std::collections::BTreeSet<u32>)>| span.is_some_and(|s| s.0 == end);
        if self.by_slot.get(&index) == Some(&start) && same(self.by_start.get(&start)) {
            return Vec::new();
        }
        self.forget(index);
        if end <= start {
            return Vec::new();
        }
        let rule = self.rule;
        // The spans the view cannot be there with.
        let clashing: Vec<u64> = match rule {
            Rule::Start => self
                .by_start
                .range(start.saturating_sub(HEAD - 1)..start.saturating_add(HEAD))
                .map(|(at, _)| *at)
                .collect(),
            Rule::Overlap | Rule::Never => self.overlapping(start, end),
        };
        let mut stale = Vec::new();
        for at in clashing {
            if at == start && same(self.by_start.get(&at)) {
                continue;
            }
            if !newer || rule == Rule::Never {
                return stale;
            }
            if let Some((old_end, slots)) = self.by_start.remove(&at) {
                // The first of them are said with both spans, to tell a
                // replacement from a neighbour in one launch's log.
                static SAID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if SAID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 48 {
                    tracing::info!(
                        slot = index,
                        start = format_args!("{start:#x}"),
                        end = format_args!("{end:#x}"),
                        stale_start = format_args!("{at:#x}"),
                        stale_end = format_args!("{old_end:#x}"),
                        stale_slots = slots.len(),
                        rule = ?rule,
                        "a placed texture view makes older views of other memory stale"
                    );
                }
                for slot in slots {
                    self.by_slot.remove(&slot);
                    stale.push(slot);
                }
            }
        }
        self.by_start.entry(start).or_insert_with(|| (end, Default::default())).1.insert(index);
        self.by_slot.insert(index, start);
        stale
    }

    /// Slot `index` holds no placed texture view any more.
    fn forget(&mut self, index: u32) {
        let Some(start) = self.by_slot.remove(&index) else {
            return;
        };
        if let Some(span) = self.by_start.get_mut(&start) {
            span.1.remove(&index);
            if span.1.is_empty() {
                self.by_start.remove(&start);
            }
        }
    }

    /// The slots whose texture's memory overlaps `[start, end)`.
    fn slots_in(&self, start: u64, end: u64) -> Vec<u32> {
        self.overlapping(start, end)
            .iter()
            .filter_map(|at| self.by_start.get(at))
            .flat_map(|span| span.1.iter().copied())
            .collect()
    }
}

/// Title memory the runtime mapped, unmapped or is about to write since a
/// texture pass last took the ranges ([`remapped`]).
static REMAPPED: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());

/// The lowest start and the highest end of any span of [`Spans`] so far
/// (the start is `u64::MAX` until there is one): what the runtime does to
/// memory outside is none of the texture pass's business.
static SPANNED: [std::sync::atomic::AtomicU64; 2] = [
    std::sync::atomic::AtomicU64::new(u64::MAX),
    std::sync::atomic::AtomicU64::new(0),
];

/// Since the start: slots whose view a newer view of other memory there
/// made stale; ranges of memory noted as mapped, unmapped or written;
/// ranges dropped because no pass took them; slots made due by such
/// ranges; and made slots whose memory was found unmapped (made null).
pub(crate) static STALE: [std::sync::atomic::AtomicU64; 5] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 5];

/// When a view the title writes makes older views of other memory stale
/// ([`Rule`]): where it starts in the block of their smallest levels, by
/// default; `VICTORY_STALE_TEXTURES=0`: never, as before;
/// `VICTORY_STALE_TEXTURES=overlap`: wherever their spans overlap, the rule
/// until run 188.
fn stale_rule() -> Rule {
    static RULE: std::sync::OnceLock<Rule> = std::sync::OnceLock::new();
    *RULE.get_or_init(|| match std::env::var("VICTORY_STALE_TEXTURES").as_deref() {
        Ok("0") => Rule::Never,
        Ok("overlap") => Rule::Overlap,
        _ => Rule::Start,
    })
}

/// The runtime mapped or unmapped title memory `[at, at + len)`, or is
/// about to write it through the kernel (`crate::watch`): the placed
/// textures there may have arrived, gone, or changed with no descriptor
/// written. The range is kept for the next texture pass
/// ([`HostHeap::texture_slots_remapped`]). Called with the runtime's own
/// locks held: this locks one mutex of its own and calls nothing.
pub(crate) fn remapped(at: usize, len: usize) {
    use std::sync::atomic::Ordering::Relaxed;
    /// Ranges kept until a pass takes them.
    const MOST: usize = 1 << 15;
    let (start, end) = (at as u64, at.saturating_add(len) as u64);
    if end <= SPANNED[0].load(Relaxed) || start >= SPANNED[1].load(Relaxed) {
        return;
    }
    let mut ranges = REMAPPED.lock().unwrap_or_else(|e| e.into_inner());
    STALE[1].fetch_add(1, Relaxed);
    // Pages are mapped one after the other.
    if let Some(last) = ranges.last_mut()
        && last.1 == start
    {
        last.1 = end;
        return;
    }
    if ranges.len() >= MOST {
        // More ranges than a pass takes at once. They were all dropped here
        // (meant for when no pass runs: no shader that picks textures by
        // index is bound), but in gameplay a load notes tens of thousands
        // between two passes (at 2-3 frames a second a pass comes every
        // 300-400 ms), and every texture written before the drop stayed as
        // its first look found it: 98,304 ranges dropped in run vg28,
        // 229,376 in vg30. Now they are joined instead, the gaps allowed
        // between them growing until half are left: a few textures near the
        // ranges are looked at once more than needed, none is left out.
        let before = ranges.len();
        coalesce(&mut ranges, MOST / 2);
        STALE[2].fetch_add((before - ranges.len()) as u64, Relaxed);
    }
    ranges.push((start, end));
}

/// Sorts `ranges` and joins them until at most `most` are left: those that
/// touch or overlap, then those less than 64 KiB apart, then 256 KiB, and
/// so on.
fn coalesce(ranges: &mut Vec<(u64, u64)>, most: usize) {
    ranges.sort_unstable();
    let mut gap = 0u64;
    loop {
        let mut joined: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
        for &(start, end) in ranges.iter() {
            match joined.last_mut() {
                Some(last) if start <= last.1.saturating_add(gap) => last.1 = last.1.max(end),
                _ => joined.push((start, end)),
            }
        }
        *ranges = joined;
        if ranges.len() <= most || gap >= 1 << 40 {
            return;
        }
        gap = if gap == 0 { 64 << 10 } else { gap * 4 };
    }
}

/// Frames for which a slot a draw read through a table counts as a table's
/// slot ([`HostHeap::tabled`]).
const TABLED_FRAMES: u64 = 240;

/// Frames a write of a table's slot waits in the queue for the draw that
/// reads it, before the slot is taken for one shaders pick by index.
/// Frames as `crate::frame::current` counts them: in its opening cutscene
/// The title begins two or three of them per frame it presents
/// (435 begun in the 353 s of run 52's, for some 140 presented).
const TABLED_GRACE: u64 = 8;

/// In [`HostHeap::tabled`]: a draw read the slot in that frame.
const TABLED_READ: u32 = 1 << 31;

/// Since the start: writes of texture views into slots that count as a
/// table's (due later), and slots that left the queue because a draw read
/// them through a table.
pub(crate) static TABLED_WRITES: [std::sync::atomic::AtomicU64; 2] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 2];

/// Whether slots draws read through tables leave the queue of textures
/// picked by index; `VICTORY_TABLED_TEXTURES=1` keeps them in it as
/// before.
fn tabled_out() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_TABLED_TEXTURES").is_none_or(|v| v != "1"))
}

/// What the queue of textures picked by index knows of a title slot.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Waiting {
    /// The frame from which the slot is due.
    due: u64,
    /// The frame it was written in.
    written: u64,
    /// Which write of the queue put it here: the pass hands it back with
    /// what it did, so a slot written again meanwhile stays as written.
    write: u64,
    /// Looks in a row that found nothing to upload (its memory not mapped,
    /// or no more of it than at the last upload).
    looks: u8,
    /// Bytes of its memory that were mapped when it was last uploaded
    /// without all of it there; 0: not uploaded yet.
    mapped: u64,
    /// For a slot uploaded whole and looked at again
    /// ([`TextureQueue::recheck`]): where its memory is (address, bytes),
    /// the fingerprint it had at the upload, how many looks found it
    /// unchanged, and the frame of the upload. A fingerprint of 0: none.
    checked: (u64, u64, u64, u8, u64),
    /// The ranges of its memory the fingerprint reads at the start of its
    /// mips (`crate::texture::staged_memory`), with `checked`.
    mips: Option<Mips>,
    /// While the card has no room: refused until this frame, and refusals
    /// in a row ([`TextureQueue::full`]). Ignored while there is room.
    refused_until: u64,
    full_waits: u8,
}

/// Ranges of a texture's memory (offsets, bytes) read for its fingerprint.
pub(crate) type Mips = std::sync::Arc<[(u64, u64)]>;

/// The queue of title slots whose texture view waits to be brought up to
/// date for shaders that pick textures by index: which are due, and in
/// which order (no host heap needed, so it is tested without one).
#[derive(Default)]
pub(crate) struct TextureQueue {
    slots: std::collections::BTreeMap<u32, Waiting>,
    writes: u64,
}

impl TextureQueue {
    /// A texture view was written into slot `index` in `frame`, due from
    /// frame `due`. `seen`: it was not written, a draw or the pass made
    /// the host slot from a view the queue may hold already, which then
    /// stays as it is.
    fn written(&mut self, index: u32, frame: u64, due: u64, seen: bool) {
        if seen && self.slots.contains_key(&index) {
            return;
        }
        self.writes += 1;
        let write = self.writes;
        self.slots.insert(index, Waiting { due, written: frame, write, looks: 0, mapped: 0, checked: (0, 0, 0, 0, 0), mips: None, refused_until: 0, full_waits: 0 });
    }

    fn remove(&mut self, index: u32) {
        self.slots.remove(&index);
    }

    /// Up to `max` slots due at `frame`, each with its write number, in
    /// the order to serve them:
    /// - first the ones looked at before (their memory was not mapped, or
    ///   only part of it): the look is a query, and memory that has
    ///   arrived since is what the title streamed in last;
    /// - then the others, the newest write first and the oldest next, in
    ///   turns: slot order served the low slots of a backlog of
    ///   25,000-40,000 while the textures of the shot on screen waited
    ///   (run 52: 98% of the drawn pixels had no colour texture in the
    ///   first 3D frames).
    ///
    /// Also counts all slots, as the frame line needs the whole backlog:
    /// queued, due, waiting for memory, partly uploaded, and the age in
    /// frames of the oldest due write.
    ///
    /// `room`: whether the card has room for new textures; without it a
    /// slot refused lately ([`TextureQueue::full`]) is left out of the
    /// order (not of the counts), so the slots behind it get their turn.
    fn due_for(&self, frame: u64, max: usize, room: bool) -> (Vec<(u32, u64)>, [u64; 5]) {
        let mut counts = [self.slots.len() as u64, 0, 0, 0, 0];
        let mut again = Vec::new();
        let mut fresh = Vec::new();
        for (&index, w) in &self.slots {
            if w.mapped > 0 {
                counts[3] += 1;
            } else if w.looks > 0 {
                counts[2] += 1;
            }
            if w.due <= frame {
                counts[1] += 1;
                counts[4] = counts[4].max(frame.saturating_sub(w.written));
                if max > 0 && (room || w.refused_until <= frame) {
                    let class = if w.looks > 0 || w.mapped > 0 {
                        &mut again
                    } else {
                        &mut fresh
                    };
                    class.push((w.write, index));
                }
            }
        }
        let retries = again.len().min(max);
        let first_retry = again.len() - retries;
        let by_write = |&(write, _): &(u64, u32)| write;
        if first_retry > 0 {
            again.select_nth_unstable_by_key(first_retry, by_write);
        }
        let again = &mut again[first_retry..];
        again.sort_unstable_by_key(by_write);
        let fresh_count = fresh.len().min(max - retries);
        let mut order = Vec::with_capacity(retries + fresh_count);
        order.extend(again.iter().rev().map(|&(write, index)| (index, write)));
        if fresh_count == 0 {
            return (order, counts);
        }

        // The write number gives the order. Only sort the ends we will
        // serve, leaving the middle of a large backlog unordered.
        let oldest_count = fresh_count / 2;
        let newest_count = fresh_count - oldest_count;
        let newest_start = fresh.len() - newest_count;
        if fresh_count == fresh.len() {
            fresh.sort_unstable_by_key(by_write);
        } else {
            fresh.select_nth_unstable_by_key(newest_start, by_write);
            if oldest_count > 0 {
                fresh[..newest_start].select_nth_unstable_by_key(oldest_count, by_write);
                fresh[..oldest_count].sort_unstable_by_key(by_write);
            }
            fresh[newest_start..].sort_unstable_by_key(by_write);
        }
        let mut oldest = fresh[..oldest_count].iter();
        for &(write, index) in fresh[newest_start..].iter().rev() {
            order.push((index, write));
            if let Some(&(write, index)) = oldest.next() {
                order.push((index, write));
            }
        }
        (order, counts)
    }

    #[cfg(test)]
    fn due(&self, frame: u64, max: usize) -> (Vec<(u32, u64)>, [u64; 5]) {
        self.due_for(frame, max, true)
    }

    /// The entry of `index` if it is still the write the pass was handed.
    fn same(&mut self, index: u32, write: u64) -> Option<&mut Waiting> {
        self.slots.get_mut(&index).filter(|w| w.write == write)
    }

    /// Slot `index` is up to date: out of the queue until written again.
    fn done(&mut self, index: u32, write: u64) {
        if self.same(index, write).is_some() {
            self.slots.remove(&index);
        }
    }

    /// Nothing to upload at slot `index` now: looked at again after 1, 2,
    /// 4, ... frames, at most `most`.
    fn later(&mut self, index: u32, write: u64, frame: u64, most: u64) {
        if let Some(w) = self.same(index, write) {
            w.due = frame + (1u64 << w.looks.min(16)).min(most.max(1));
            w.looks = w.looks.saturating_add(1);
            (w.refused_until, w.full_waits) = (0, 0);
        }
    }

    /// Slot `index` was refused because the card has no room: while that
    /// lasts it gives way to the other due slots for 2, 4, 8, ... frames,
    /// at most `most`; `due` and `looks` stay. (Ported from fork PR 676
    /// f130ba1a by AscilCH: the same first 257 refused slots ended every
    /// pass, and the slots behind them never had a turn.)
    fn full(&mut self, index: u32, write: u64, frame: u64, most: u64) {
        if let Some(w) = self.same(index, write) {
            w.refused_until = frame.saturating_add((2u64 << w.full_waits.min(7)).min(most.max(1)));
            w.full_waits = w.full_waits.saturating_add(1);
        }
    }

    /// Slot `index` was uploaded with `mapped` bytes of its memory there,
    /// not all of it: looked at again from frame `due`.
    fn partly(&mut self, index: u32, write: u64, due: u64, mapped: u64) {
        if let Some(w) = self.same(index, write) {
            (w.due, w.looks, w.mapped) = (due, 0, mapped.max(1));
            (w.refused_until, w.full_waits) = (0, 0);
        }
    }

    /// Slot `index` was uploaded whole from memory `at` (address, bytes)
    /// with fingerprint `print`: looked at again from frame `due`, for a
    /// title that fills a texture's memory after it wrote the view (the
    /// `looks`-th such look; `uploaded` is the upload's frame).
    /// `mips`: the ranges the fingerprint read besides its windows.
    #[allow(clippy::too_many_arguments)]
    fn recheck(&mut self, index: u32, write: u64, due: u64, at: (u64, u64), mips: Option<Mips>, print: u64, looks: u8, uploaded: u64) {
        if let Some(w) = self.same(index, write) {
            (w.due, w.looks, w.mapped) = (due, 1, 0);
            w.checked = (at.0, at.1, print, looks, uploaded);
            w.mips = mips;
            (w.refused_until, w.full_waits) = (0, 0);
        }
    }

    /// What [`TextureQueue::recheck`] noted of slot `index`, if it is
    /// waiting for such a look.
    fn checked(&self, index: u32) -> Option<((u64, u64, u64, u8, u64), Option<Mips>)> {
        self.slots.get(&index).filter(|w| w.checked.2 != 0).map(|w| (w.checked, w.mips.clone()))
    }

    /// The memory slot `index`'s texture lies in was mapped, unmapped or
    /// written in `frame`: the slot is looked at from frame `due` as one
    /// written now, whatever was known of it (a pass at work on it does
    /// not take it out).
    fn changed(&mut self, index: u32, frame: u64, due: u64) {
        self.writes += 1;
        let write = self.writes;
        let due = self.slots.get(&index).map_or(due, |w| w.due.min(due));
        self.slots.insert(index, Waiting { due, written: frame, write, looks: 0, mapped: 0, checked: (0, 0, 0, 0, 0), mips: None, refused_until: 0, full_waits: 0 });
    }

    /// Bytes of slot `index`'s memory that were mapped at its last upload
    /// (0: not uploaded, or all of it was).
    fn mapped(&self, index: u32) -> u64 {
        self.slots.get(&index).map_or(0, |w| w.mapped)
    }

    /// [`HostHeap::texture_slots_bake_prepare`]: slots waiting for their
    /// first look after a whole upload, and slots uploaded in part, are
    /// due in `frame`. How many.
    fn bake_prepare(&mut self, frame: u64) -> usize {
        let mut pulled = 0;
        for w in self.slots.values_mut() {
            if w.due > frame && ((w.checked.2 != 0 && w.checked.3 <= 1) || w.mapped > 0) {
                w.due = frame;
                pulled += 1;
            }
        }
        pulled
    }
}

/// Whether `view` is of a texture the title may fill itself: a texture it
/// placed in memory, or a texture object.
pub(crate) fn is_texture_view(view: &View) -> bool {
    match view {
        View::Placed(false, resource, _) => resource[0] != 1,
        View::Shader(r, _) => crate::resource::buffer_bytes(*r as crate::object::This).is_none(),
        _ => false,
    }
}

/// Buffer views left null because the host would refuse them
/// ([`buffer_view_ok`]), since the start.
pub(crate) static REFUSED_VIEWS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Whether the host device is gone, and why (the first time it is asked
/// after that): for passes that make many views, to name the one that did
/// it.
pub(crate) fn device_removed() -> Option<String> {
    let host = host()?;
    // SAFETY: a plain query.
    let reason = unsafe { host.device.GetDeviceRemovedReason() };
    reason.err().map(|e| e.to_string())
}

/// Writes of title view-heap slots with a placed texture view and with a
/// view of a texture object, since the start (diagnostic counts).
pub(crate) static PLACED_TEXTURE_SLOTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub(crate) static OBJECT_TEXTURE_SLOTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Bytes per element of a buffer view, as the console's descriptor holds
/// them for indexed reads (structured: the stride; raw: 1, the index is the
/// byte; typed: the format's); `None` if `view` is not a buffer view.
fn view_stride(view: &View) -> Option<u32> {
    const BUFFER: u32 = 1;
    let (words, unordered): (&[u32; 10], bool) = match view {
        View::Shader(_, Some(d)) if d[1] == BUFFER => (d, false),
        View::Unordered(_, _, Some(d)) if d[1] == BUFFER => (d, true),
        View::Placed(unordered, resource, d) if resource[0] == BUFFER => {
            (d[..10].try_into().ok()?, *unordered)
        }
        _ => return None,
    };
    // SAFETY: buffer views' union members.
    let (stride, raw, format) = unsafe {
        if unordered {
            let d = uav_desc(words);
            let b = d.Anonymous.Buffer;
            (b.StructureByteStride, b.Flags.0 & D3D12_BUFFER_UAV_FLAG_RAW.0 != 0, d.Format)
        } else {
            let d = srv_desc(words);
            let b = d.Anonymous.Buffer;
            (b.StructureByteStride, b.Flags.0 & D3D12_BUFFER_SRV_FLAG_RAW.0 != 0, d.Format)
        }
    };
    Some(if stride > 0 {
        stride
    } else if raw {
        1
    } else {
        crate::resource::element(format.0 as u32).map_or(0, |(bytes, _)| bytes as u32)
    })
}

/// Host heaps' GPU ranges (start, bytes, bytes per slot, sampler), for
/// device-removed reports: a shader indexing a heap past its end faults.
static GPU_RANGES: Mutex<Vec<(u64, u64, u64, bool)>> = Mutex::new(Vec::new());

/// For device-removed reports: where host GPU address `va` lies against
/// the host descriptor heaps.
pub(crate) fn near_heaps(va: u64) -> String {
    let ranges = GPU_RANGES.lock().unwrap_or_else(|e| e.into_inner());
    let mut text = String::new();
    for &(start, bytes, step, sampler) in ranges.iter() {
        let kind = if sampler { "sampler" } else { "view" };
        let place = if va < start {
            format!("{:#x} below", start - va)
        } else if va < start + bytes {
            format!("inside, slot {}", (va - start) / step)
        } else {
            format!("{:#x} past its end (slot {})", va - start - bytes, (va - start) / step)
        };
        text.push_str(&format!("; {kind} heap {start:#x}+{bytes:#x}: {place}"));
    }
    text
}

/// The host's limit for shader-visible view heaps on resource binding
/// tiers 1 and 2; tier 3 hardware may take more.
const PORTABLE_VIEWS: u32 = 1_000_000;

/// The size of every host view heap on a host that takes more than
/// [`PORTABLE_VIEWS`] (AMD): the title's heap (a little over 1,000,000
/// views in the retail game) plus [`DYNAMIC`] fits with room to spare, and
/// the last slot is the null slot every view index at or past it reads.
const UNCAPPED_VIEWS: u32 = 1 << 21;

/// Picks, once, the null slot at the end of every host view heap and tells
/// the translator now (`set_view_heap_limit`) instead of when the title's
/// heap is made: a shader keeps the limit it was translated with, and the
/// title makes 25 pipelines (their shaders translated at once) before it
/// makes its view heap. Called before a shader is translated and before a
/// view heap is made.
///
/// Every host clamps. A host that caps view heaps at [`PORTABLE_VIEWS`]
/// (NVIDIA) hung on a view index past the heap; one that does not (AMD
/// RDNA3, RX 7900 XT) took a page fault and removed the device on entering
/// the world: a compute program read view slot 16,777,088 (0xffff80) of a
/// heap of 1,507,328, past its end. Unclamped, that index is undefined on
/// any host.
pub(crate) fn probe_view_limit() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Some(host) = host() else { return };
        let fits = |count: u32| {
            // SAFETY: plain heap creation; the heap is released at once.
            let made: windows::core::Result<ID3D12DescriptorHeap> = unsafe {
                host.device
                    .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                        Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                        NumDescriptors: count,
                        Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                        NodeMask: 0,
                    })
            };
            made.is_ok()
        };
        let views = if fits(UNCAPPED_VIEWS) {
            UNCAPPED_VIEWS
        } else {
            PORTABLE_VIEWS
        };
        tracing::info!(
            views,
            "host view heaps hold {views} views; view indices are clamped to the null slot at the end from the first shader on"
        );
        victory_shader::emit::set_view_heap_limit(views - 1);
    });
}

/// Slots past the title's in a view heap, for tables a draw needs made
/// afresh (constant buffers copied per draw). A list holds the slots it
/// took until it is reset (its submissions have run) or released.
/// 2^16 ran out in the world ("no free dynamic descriptor slots", run 11;
/// upstream PR #336 takes 2^18). On a host heap capped at 1,000,000 views
/// (NVIDIA) these come out of the title's slots, whose highest seen is
/// about 720,000: 2^17 left 868,000 of them, and ran out in the opening
/// cutscene (a frame asks for up to 16,000 tables while the frame before
/// still holds its own; a draw refused its table read slots never made
/// for it, and the card stopped within frames). 2^18 leaves 737,000; a
/// table past them is made afresh in these slots like any other.
pub(crate) const DYNAMIC: u32 = 1 << 18;

/// The dynamic slots of a view heap, read once: [`DYNAMIC`], or
/// `VICTORY_DYNAMIC_SLOTS` from 131,072 to 524,288. On a host heap capped
/// at 1,000,000 views (NVIDIA) every slot more is a title slot fewer: past
/// about 280,000 the title's highest seen (about 720,000) has none.
/// Translated shaders and their cache keys do not depend on it (they know
/// only the null slot, `probe_view_limit`).
fn dynamic_slots() -> u32 {
    static SLOTS: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *SLOTS.get_or_init(|| {
        let Some(asked) = std::env::var_os("VICTORY_DYNAMIC_SLOTS") else {
            return DYNAMIC;
        };
        match asked.to_str().and_then(|s| s.trim().parse::<u32>().ok()) {
            Some(n) if (1 << 17..=1 << 19).contains(&n) => {
                tracing::info!(slots = n, "dynamic descriptor slots set (VICTORY_DYNAMIC_SLOTS)");
                n
            }
            _ => {
                tracing::warn!(
                    asked = ?asked,
                    slots = DYNAMIC,
                    "VICTORY_DYNAMIC_SLOTS takes 131072 to 524288: the default is used"
                );
                DYNAMIC
            }
        }
    })
}

/// Whether single dynamic slots are taken from the top of the ring down and
/// longer tables from the bottom up, read once: on unless
/// `VICTORY_DYNAMIC_SPLIT=0` (one search from the bottom up for all, as
/// before). In the world of the title some 30,000 tables a frame, many of
/// one slot, were taken among the longer ones: when those were given back,
/// the single slots still held cut their runs in pieces, and 1,445 tables of
/// a frame were refused with enough slots free.
fn dynamic_split() -> bool {
    static SPLIT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SPLIT.get_or_init(|| {
        let split = !matches!(std::env::var("VICTORY_DYNAMIC_SPLIT").as_deref().map(str::trim), Ok("0"));
        if !split {
            tracing::info!("dynamic descriptor slots: one search for all tables (VICTORY_DYNAMIC_SPLIT=0)");
        }
        split
    })
}

fn dynamic_pack() -> bool {
    static PACK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PACK.get_or_init(|| {
        let pack = !matches!(std::env::var("VICTORY_DYNAMIC_PACK").as_deref().map(str::trim), Ok("0"));
        tracing::info!(pack, "dynamic descriptor packing (VICTORY_DYNAMIC_PACK)");
        pack
    })
}

/// The dynamic slots of a view heap: which are taken, and where the
/// searches for free ones start. Every search looks at the whole ring, so a
/// table is refused only when no run of free slots is long enough
/// (`HostHeap::too_long` relies on that).
///
/// The taken flags are bits of 64-bit words, so a search passes a word of
/// taken slots at once and finds the ends of runs by counting zeros: it had
/// walked a flag per slot, 30.6 ms a frame of the world's under the lock
/// all recording threads share.
struct Ring {
    /// When enabled the coalesced runs replace bitmap searches.
    free_runs: Option<FreeRuns>,
    /// Slots owned by submitted flights, in host fence order. They are
    /// removed exactly once, either by landing or by an allocation.
    retiring: std::collections::VecDeque<(u64, Vec<(u32, u32)>)>,
    /// Where the search for a table starts, upward.
    next: usize,
    /// Where the search for a single slot starts, downward, when single
    /// slots are split from longer tables.
    top: usize,
    /// Slots in the ring.
    len: usize,
    /// Bit `i % 64` of word `i / 64`: slot `i` is taken. The bits past
    /// `len` in the last word are set (taken for good), so no run of free
    /// slots reaches past the end.
    words: Vec<u64>,
    /// One recently freed run per power of two. The bitmap remains authoritative.
    holes: [(usize, usize); usize::BITS as usize],
    pack: bool,
    /// High slots returned above the downward cursor.
    released_top: usize,
}

#[derive(Default)]
struct FreeRuns {
    // Both indices describe the same disjoint maximal free runs.
    ends: std::collections::BTreeMap<usize, usize>,
    sizes: std::collections::BTreeSet<(usize, usize)>,
}

/// Consecutive reservations of a flight often abut, in either allocation
/// direction. Only join exact neighbours owned by this completed flight.
fn coalesced(ranges: Vec<(u32, u32)>) -> impl Iterator<Item = (u32, u32)> {
    let mut ranges = ranges.into_iter().peekable();
    std::iter::from_fn(move || {
        let (mut at, mut n) = ranges.next()?;
        while let Some(&(next, len)) = ranges.peek() {
            if at + n == next {
                n += len;
            } else if next + len == at {
                at = next;
                n += len;
            } else {
                break;
            }
            ranges.next();
        }
        Some((at, n))
    })
}

impl FreeRuns {
    fn new(len: usize) -> Self {
        let mut runs = Self::default();
        runs.insert(0, len);
        runs
    }

    fn insert(&mut self, start: usize, end: usize) {
        if start < end {
            self.ends.insert(start, end);
            self.sizes.insert((end - start, start));
        }
    }

    fn remove(&mut self, start: usize, end: usize) {
        self.ends.remove(&start);
        self.sizes.remove(&(end - start, start));
    }

    fn take(&mut self, n: usize, down: bool) -> Option<usize> {
        let &(len, start) = self.sizes.range((n, 0)..).next()?;
        let end = start + len;
        self.remove(start, end);
        if down {
            self.insert(start, end - n);
            Some(end - n)
        } else {
            self.insert(start + n, end);
            Some(start)
        }
    }

    fn release(&mut self, at: usize, end: usize) -> (u32, usize) {
        let mut given = end - at;
        let mut start = at;
        let mut stop = end;
        if let Some((&lo, &hi)) = self.ends.range(..at).next_back()
            && hi >= at
        {
            self.remove(lo, hi);
            given -= hi.min(end) - at;
            start = lo;
            stop = stop.max(hi);
        }
        while let Some((&lo, &hi)) = self.ends.range(at..=end).next() {
            self.remove(lo, hi);
            given -= hi.min(end) - lo;
            stop = stop.max(hi);
        }
        self.insert(start, stop);
        (given as u32, stop - start)
    }
}

impl Ring {
    fn new(len: usize, pack: bool) -> Self {
        let mut words = vec![0u64; len.div_ceil(64)];
        let past = len % 64;
        if past != 0 {
            words[len / 64] = !0u64 << past;
        }
        Ring {
            free_runs: None,
            retiring: Default::default(),
            next: 0,
            top: len.saturating_sub(1),
            len,
            words,
            holes: [(0, 0); usize::BITS as usize],
            pack,
            released_top: 0,
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn completed_ranges(&mut self, completed: u64) -> Option<Vec<(u32, u32)>> {
        if completed == u64::MAX || self.retiring.front()?.0 > completed {
            return None;
        }
        self.retiring.pop_front().map(|(_, ranges)| ranges)
    }

    /// Word `w` with the bits of the slots sought set: free ones (`free`)
    /// or taken ones.
    fn word(&self, w: usize, free: bool) -> u64 {
        if free { !self.words[w] } else { self.words[w] }
    }

    /// The first slot in `at..limit` that is free (`free`) or taken;
    /// `limit` if none is. `limit` is at most the ring's length.
    fn next_bit(&self, at: usize, limit: usize, free: bool) -> usize {
        if at >= limit {
            return limit;
        }
        let mut w = at / 64;
        let mut bits = self.word(w, free) & (!0u64 << (at % 64));
        loop {
            if bits != 0 {
                return (w * 64 + bits.trailing_zeros() as usize).min(limit);
            }
            w += 1;
            if w * 64 >= limit {
                return limit;
            }
            bits = self.word(w, free);
        }
    }

    /// The last slot in `lo..=from` that is free (`free`) or taken. `from`
    /// is less than the ring's length.
    fn prev_bit(&self, from: usize, lo: usize, free: bool) -> Option<usize> {
        if from < lo {
            return None;
        }
        let mut w = from / 64;
        let below = from % 64;
        let mask = if below == 63 { !0u64 } else { (1u64 << (below + 1)) - 1 };
        let mut bits = self.word(w, free) & mask;
        loop {
            if bits != 0 {
                let at = w * 64 + 63 - bits.leading_zeros() as usize;
                return (at >= lo).then_some(at);
            }
            if w * 64 <= lo {
                return None;
            }
            w -= 1;
            bits = self.word(w, free);
        }
    }

    /// Calls `each` with the index and the mask of every word the slots
    /// `at..end` lie in.
    fn masks(at: usize, end: usize, mut each: impl FnMut(usize, u64)) {
        if at >= end {
            return;
        }
        let (first, last) = (at / 64, (end - 1) / 64);
        for w in first..=last {
            let lo = if w == first { at % 64 } else { 0 };
            let hi = if w == last { (end - 1) % 64 + 1 } else { 64 };
            let upto = if hi == 64 { !0u64 } else { (1u64 << hi) - 1 };
            each(w, upto & (!0u64 << lo));
        }
    }

    /// Takes `n` (at least one) consecutive free slots: the first's index.
    /// `split`: a single slot is searched for from the top down.
    fn take(&mut self, n: usize, split: bool) -> Option<usize> {
        if n == 0 || n > self.len() {
            return None;
        }
        let down = split && n == 1;
        if let Some(runs) = &mut self.free_runs {
            return runs.take(n, down);
        }
        let recycled = self.recycled(n, down);
        let first = match recycled {
            Some(at) => at,
            None if down => self.find_down()?,
            None => self.find_up(n)?,
        };
        let words = &mut self.words;
        Self::masks(first, first + n, |w, mask| words[w] |= mask);
        if recycled.is_some() {
            // Leave the main cursors at the unallocated frontiers.
            #[cfg(feature = "diagnostics")]
            crate::copystat::packed_request();
        } else if down {
            self.top = first.checked_sub(1).unwrap_or(self.len() - 1);
        } else {
            self.next = (first + n) % self.len();
        }
        Some(first)
    }

    /// Fill a recently returned hole before taking a bite out of a longer run.
    fn recycled(&mut self, n: usize, down: bool) -> Option<usize> {
        if !self.pack {
            return None;
        }
        if down && self.released_top > self.top {
            if let Some(at) = self.prev_bit(self.released_top, self.top + 1, true) {
                self.released_top = at - 1;
                return Some(at);
            }
            self.released_top = 0;
        }
        let class = n.ilog2() as usize;
        // Only try similarly sized holes. Split singles prefer isolated slots.
        for i in class..=(class + 1).min(self.holes.len() - 1) {
            let (at, len) = self.holes[i];
            if len < n || len - n > n || (down && len != 1) {
                continue;
            }
            self.holes[i] = (0, 0);
            let end = at + len;
            // A later allocation or release may have changed either boundary.
            if (at > 0 && !self.is_taken(at - 1))
                || (end < self.len && !self.is_taken(end))
                || self.next_bit(at, end, false) != end
            {
                continue;
            }
            if len > n {
                self.remember(at + n, end);
            }
            return Some(at);
        }
        None
    }

    fn remember(&mut self, start: usize, end: usize) {
        let len = end - start;
        self.holes[len.ilog2() as usize] = (start, len);
    }

    #[cfg(any(test, feature = "diagnostics"))]
    fn largest_free_run(&self) -> usize {
        if let Some(runs) = &self.free_runs {
            return runs.sizes.last().map_or(0, |&(len, _)| len);
        }
        let mut longest = 0;
        let mut start = self.next_bit(0, self.len, true);
        while start < self.len {
            let end = self.next_bit(start, self.len, false);
            longest = longest.max(end - start);
            start = self.next_bit(end, self.len, true);
        }
        longest
    }

    /// The first run of `n` free slots from `next` upward, then from the
    /// start: its first slot. A run does not wrap past the end.
    fn find_up(&self, n: usize) -> Option<usize> {
        // Search from `next` on, then from the start (extending past cursor by
        // n - 1 so contiguous runs crossing the cursor are not missed).
        // Ported from Xeltax/VIcky 28c94c11.
        let cursor = self.next;
        let second_end = cursor.saturating_add(n.saturating_sub(1)).min(self.len());
        self.first_run(cursor, self.len(), n)
            .or_else(|| self.first_run(0, second_end, n))
    }

    /// The first run of `n` free slots that starts at `from` or later and
    /// ends at `to` or before: its first slot.
    fn first_run(&self, from: usize, to: usize, n: usize) -> Option<usize> {
        let mut at = from;
        while at + n <= to {
            let start = self.next_bit(at, to, true);
            if start + n > to {
                return None;
            }
            let stop = self.next_bit(start, start + n, false);
            if stop == start + n {
                return Some(start);
            }
            at = stop + 1;
        }
        None
    }

    /// The first free slot from `top` downward, then from the end down.
    fn find_down(&self) -> Option<usize> {
        let last = self.len().checked_sub(1)?;
        let top = self.top.min(last);
        self.prev_bit(top, 0, true)
            .or_else(|| self.prev_bit(last, top + 1, true))
    }

    /// Gives back the slots from `at` to `end` (exclusive): how many of
    /// them were taken, and the length of the run of free slots they now
    /// lie in (0 for no slots); `None` if the range is not in the ring.
    fn release(&mut self, at: usize, end: usize) -> Option<(u32, usize)> {
        if at > end || end > self.len() {
            return None;
        }
        if let Some(runs) = &mut self.free_runs {
            return Some(if at == end { (0, 0) } else { runs.release(at, end) });
        }
        let mut given = 0;
        let words = &mut self.words;
        Self::masks(at, end, |w, mask| {
            given += (words[w] & mask).count_ones();
            words[w] &= !mask;
        });
        if at == end {
            return Some((given, 0));
        }
        let start = match at.checked_sub(1) {
            Some(before) => self.prev_bit(before, 0, false).map_or(0, |taken| taken + 1),
            None => 0,
        };
        let stop = self.next_bit(end, self.len(), false);
        if given != 0 && self.pack {
            self.remember(start, stop);
            // Reuse high slots before singles drift down between live tables.
            self.released_top = self.released_top.max(stop - 1);
        }
        Some((given, stop - start))
    }

    /// Whether slot `at` is taken.
    fn is_taken(&self, at: usize) -> bool {
        if let Some(runs) = &self.free_runs {
            return runs.ends.range(..=at).next_back().is_none_or(|(_, &end)| end <= at);
        }
        (self.words[at / 64] >> (at % 64)) & 1 != 0
    }
}

/// Whether the pass over the heap's buffer views leaves the slots it makes
/// itself unmarked ([`passing`]), read once: off unless
/// `VICTORY_HEAP_PASS_UNMARKED=1`. Taken from triippywhale/VIctory 64e58c46
/// (author "Claude"), which measured 18 ms a frame of looks saved. Off by
/// default because here the self-marks also had every slot the pass left
/// as an orphan (a buffer view with no host copy yet) looked at again the
/// next pass; unmarked, an orphan waits for its slot to be written or for
/// the orphan pass every 16 passes (`crate::heap_pass`).
fn heap_pass_unmarked() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let on = matches!(std::env::var("VICTORY_HEAP_PASS_UNMARKED").as_deref().map(str::trim), Ok("1"));
        if on {
            tracing::info!("the heap's buffer-view pass does not mark the slots it makes (VICTORY_HEAP_PASS_UNMARKED=1)");
        }
        on
    })
}

thread_local! {
    /// This thread is in the pass over the heap's buffer views ([`passing`]).
    static PASSING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Until the value returned is dropped, this thread's [`HostHeap::mirror`]
/// calls are the looks of the pass over the heap's buffer views
/// (`crate::draw`'s `heap_buffers_ready`): the slots they make are not
/// marked as changed for the next pass.
pub(crate) fn passing() -> impl Drop {
    struct Passing;
    impl Drop for Passing {
        fn drop(&mut self) {
            PASSING.with(|p| p.set(false));
        }
    }
    PASSING.with(|p| p.set(heap_pass_unmarked() || crate::heap_pass::no_self_mark()));
    Passing
}

impl HostHeap {
    /// Get the sharded mutex for a slot index.
    #[inline]
    fn made_shard(&self, index: u32) -> &Mutex<Vec<u64>> {
        &self.made[(index as usize) & (MADE_SHARDS - 1)]
    }

    /// Get the sharded mutex for note_buffer operations.
    #[inline]
    fn note_shard(&self, index: u32) -> usize {
        (index as usize) & (NOTE_SHARDS - 1)
    }

    #[inline]
    fn placed_shard(&self, index: u32) -> &Mutex<Spans> {
        &self.placed[self.note_shard(index)]
    }

    #[inline]
    fn textures_shard(&self, index: u32) -> &Mutex<TextureQueue> {
        &self.textures[self.note_shard(index)]
    }

    #[inline]
    fn buffers_shard(&self, index: u32) -> &Mutex<std::collections::BTreeSet<u32>> {
        &self.buffers[self.note_shard(index)]
    }

    /// Iterates over all texture shards, calling `f` for each.
    fn each_texture_shard<F: FnMut(&Mutex<TextureQueue>)>(&self, mut f: F) {
        for shard in &self.textures {
            f(shard);
        }
    }

    /// Iterates over all buffer shards, calling `f` for each.
    fn each_buffer_shard<F: FnMut(&Mutex<std::collections::BTreeSet<u32>>)>(&self, mut f: F) {
        for shard in &self.buffers {
            f(shard);
        }
    }

    pub(crate) fn new(sampler: bool, count: u32) -> Option<Self> {
        let host = host()?;
        let kind = if sampler {
            D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER
        } else {
            D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV
        };
        let make = |count: u32| -> windows::core::Result<ID3D12DescriptorHeap> {
            // SAFETY: plain heap creation.
            unsafe {
                host.device
                    .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                        Type: kind,
                        NumDescriptors: count,
                        Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                        NodeMask: 0,
                    })
            }
        };
        let slots = dynamic_slots();
        let extra = if sampler { 0 } else { slots };
        // A view heap ends at the null slot shaders clamp view indices to
        // (`probe_view_limit`), whatever the title's own count: the title's
        // slots, then the dynamic ones, then null up to and with that slot.
        if !sampler {
            probe_view_limit();
        }
        let null = if sampler {
            0
        } else {
            victory_shader::emit::view_heap_limit()
        };
        // On a heap capped at 1,000,000 views every ring slot is a title
        // slot fewer; past the default the title's highest (about 720,000)
        // may not fit.
        if !sampler && null != 0 && null < PORTABLE_VIEWS && slots > DYNAMIC {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                tracing::warn!(
                    slots,
                    title_slots = null - slots.min(null),
                    "VICTORY_DYNAMIC_SLOTS is over 262144 on a view heap capped at 1,000,000: title slots from {} on may fall past the end and read a null slot",
                    null - slots.min(null)
                );
            });
        }
        let (heap, count, size) = if null == 0 {
            match make(count + extra) {
                Ok(heap) => (heap, count, count + extra),
                Err(e) => {
                    tracing::warn!(error = %e, count, sampler, "no host descriptor heap");
                    return None;
                }
            }
        } else {
            let size = null + 1;
            let fitted = if count + extra <= null {
                count
            } else {
                // shortcut: descriptors past the host's limit are left out; a title that reaches
                // them needs remapping. They read the null slot instead.
                tracing::warn!(count, "host view heap capped at {size}; views past it read a null slot");
                null - extra
            };
            match make(size) {
                Ok(heap) => (heap, fitted, size),
                Err(e) => {
                    tracing::warn!(error = %e, count, size, "no host view heap");
                    return None;
                }
            }
        };
        // SAFETY: a live heap.
        let (cpu, step) = unsafe {
            (
                heap.GetCPUDescriptorHandleForHeapStart().ptr,
                host.device.GetDescriptorHandleIncrementSize(kind),
            )
        };
        tracing::debug!(count, sampler, "host descriptor heap");
        // SAFETY: a live shader-visible heap.
        let gpu = unsafe { heap.GetGPUDescriptorHandleForHeapStart().ptr };
        GPU_RANGES.lock().unwrap_or_else(|e| e.into_inner()).push((
            gpu,
            u64::from(size) * u64::from(step),
            u64::from(step),
            sampler,
        ));
        let strides = (!sampler)
            .then(|| {
                let bytes = u64::from(victory_shader::emit::SLOT_STRIDES_COUNT) * 4;
                let (buffer, data) = host.upload_buffer(bytes)?;
                // SAFETY: the buffer's whole mapping; a live buffer.
                let gpu = unsafe {
                    std::ptr::write_bytes(data, 0, bytes as usize);
                    buffer.GetGPUVirtualAddress()
                };
                crate::host::name(&buffer, || "element strides by view heap slot".to_owned());
                Some((buffer, gpu, data as usize))
            })
            .flatten();
        let h = HostHeap {
            heap,
            cpu,
            step,
            count,
            dynamic: extra,
            sampler,
            made: (0..MADE_SHARDS).map(|_| Mutex::new(vec![0; count as usize])).collect(),
            made_now: (0..count).map(|_| std::sync::atomic::AtomicU64::new(0)).collect(),
            made_view: Mutex::new(vec![None; count as usize]),
            invalid_cache: (0..count).map(|_| std::sync::atomic::AtomicU8::new(0)).collect(),
            kept: (0..(count + extra).div_ceil(KEPT_PIECE)).map(|_| Mutex::new(vec![None; KEPT_PIECE as usize])).collect(),
            next: Mutex::new({
                let mut ring = Ring::new(slots as usize, dynamic_pack());
                // Best-fit search of free runs: off by default
                // (`VICTORY_DYNAMIC_BEST_FIT=1` on). A run measured it at
                // 72-83 ms a frame against 10 ms without (2026-11-25).
                if !sampler && ring.pack
                    && std::env::var_os("VICTORY_DYNAMIC_BEST_FIT").is_some_and(|v| v != "0")
                {
                    ring.free_runs = Some(FreeRuns::new(ring.len));
                }
                ring
            }),
            split: dynamic_split(),
            free: std::sync::atomic::AtomicU32::new(slots),
            too_long: std::sync::atomic::AtomicU32::new(u32::MAX),
            strides,
            buffers: (0..NOTE_SHARDS).map(|_| Mutex::new(std::collections::BTreeSet::new())).collect(),
            changed: Mutex::default(),
            textures: (0..NOTE_SHARDS).map(|_| Mutex::new(TextureQueue::default())).collect(),
            tabled: (0..if sampler { 0 } else { count })
                .map(|_| std::sync::atomic::AtomicU32::new(0))
                .collect(),
            placed: (0..NOTE_SHARDS).map(|_| Mutex::new(Spans::default())).collect::<Vec<_>>(),
        };
        // A slot never written is undefined: shaders reading one (a slot
        // the title wrote in a way we do not know, or a table entry no
        // used-resource entry names) read a wild address. Every slot starts
        // null instead.
        let started = std::time::Instant::now();
        if let Err(e) = h.fill_null(kind, size) {
            tracing::warn!(error = %e, "host descriptor heap not made null");
        }
        tracing::debug!(ms = started.elapsed().as_millis(), "host descriptor heap made null");
        Some(h)
    }

    /// Makes the first `total` slots null: one null slot in a CPU-side heap,
    /// doubled by copies to a block, the block copied over the heap.
    fn fill_null(&self, kind: D3D12_DESCRIPTOR_HEAP_TYPE, total: u32) -> windows::core::Result<()> {
        const BLOCK: u32 = 1 << 16;
        let Some(host) = host() else { return Ok(()) };
        // SAFETY: plain heap creation and copies inside live heaps.
        unsafe {
            let block: ID3D12DescriptorHeap =
                host.device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: kind,
                    NumDescriptors: BLOCK,
                    ..Default::default()
                })?;
            let start = block.GetCPUDescriptorHandleForHeapStart();
            let at = |i: u32| D3D12_CPU_DESCRIPTOR_HANDLE {
                ptr: start.ptr + (i * self.step) as usize,
            };
            if self.sampler {
                null_sampler(start);
            } else {
                null_view(start);
            }
            let mut filled = 1;
            while filled < BLOCK {
                let n = filled.min(BLOCK - filled);
                host.device.CopyDescriptorsSimple(n, at(filled), start, kind);
                filled += n;
            }
            let mut done = 0;
            while done < total {
                let n = BLOCK.min(total - done);
                host.device.CopyDescriptorsSimple(n, self.cpu(done), start, kind);
                done += n;
            }
        }
        Ok(())
    }

    /// A plain point sampler at slot `index`, for sampler slots without a
    /// view.
    fn null_sampler(&self, index: u32) {
        null_sampler(self.cpu(index));
    }
}

/// A plain point sampler at `at`.
fn null_sampler(at: D3D12_CPU_DESCRIPTOR_HANDLE) {
    let Some(host) = host() else { return };
    let desc = D3D12_SAMPLER_DESC {
        Filter: D3D12_FILTER_MIN_MAG_MIP_POINT,
        AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        MaxAnisotropy: 1,
        ComparisonFunc: D3D12_COMPARISON_FUNC_NEVER,
        ..Default::default()
    };
    // SAFETY: a slot of a sampler heap of ours.
    unsafe { host.device.CreateSampler(&desc, at) };
}

impl HostHeap {
    /// `n` consecutive free dynamic slots (view heaps only), taken until
    /// the list discards them or its submission has finished.
    pub(crate) fn dynamic(&self, n: u32) -> Option<u32> {
        if self.sampler || n == 0 || n > self.dynamic {
            return None;
        }
        #[cfg(feature = "diagnostics")]
        let started = std::time::Instant::now();
        #[cfg(feature = "diagnostics")]
        let mut waiting = std::time::Duration::ZERO;
        #[cfg(feature = "diagnostics")]
        {
            crate::copystat::add(crate::copystat::C::DynamicAsked, 1);
            crate::copystat::add(crate::copystat::C::DynamicSlots, u64::from(n));
        }
        let mut ring = self.next.lock().unwrap_or_else(|e| e.into_inner());
        // Bound a pressure wait to flights already submitted on entry.
        // Recording, async-submit work and held title waits have no fence.
        let mut last = ring.retiring.back().map_or(0, |p| p.0);
        #[cfg(feature = "diagnostics")]
        let mut retried = false;
        let mut free;
        let found = loop {
            free = self.free.load(std::sync::atomic::Ordering::Relaxed);
            if free >= n && n < self.too_long.load(std::sync::atomic::Ordering::Relaxed) {
                if let Some(first) = ring.take(n as usize, self.split) {
                    break Some(first);
                }
                self.too_long.fetch_min(n, std::sync::atomic::Ordering::Relaxed);
            }
            let Some(target) = ring.retiring.front().map(|p| p.0).filter(|f| *f <= last) else {
                break None;
            };
            drop(ring);
            #[cfg(feature = "diagnostics")]
            let before = std::time::Instant::now();
            let completed = crate::host::dynamic_wait(target);
            #[cfg(feature = "diagnostics")]
            {
                retried = true;
                waiting += before.elapsed();
            }
            ring = self.next.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(completed) = completed {
                self.release_completed_locked(&mut ring, completed);
            } else {
                last = 0; // One final search for slots returned during the wait.
            }
        };
        #[cfg(feature = "diagnostics")]
        {
            use crate::copystat::{C, add};
            crate::copystat::dynamic_free(free, n);
            add(C::DynamicWaitNs, waiting.as_nanos() as u64);
            add(C::DynamicNs, (started.elapsed() - waiting).as_nanos() as u64);
        }
        let Some(first) = found else {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| tracing::warn!(n, "no free dynamic descriptor slots: a table is not made"));
            #[cfg(feature = "diagnostics")]
            {
                use crate::copystat::{C, add};
                add(C::DynamicRefused, 1);
                crate::copystat::sample_refused_ring(|| ring.largest_free_run());
                add(
                    if free >= n { C::DynamicFragmented } else { C::DynamicExhausted },
                    1,
                );
            }
            return None;
        };
        let _was = self.free.fetch_sub(n, std::sync::atomic::Ordering::Relaxed);
        #[cfg(feature = "diagnostics")]
        {
            crate::copystat::dynamic_free(_was - n, n);
            crate::copystat::dynamic_taken(n);
            if retried {
                crate::copystat::add(crate::copystat::C::DynamicRecovered, 1);
            }
        }
        Some(self.count + first as u32)
    }

    /// The title slots this heap has host slots for; fewer than the title's
    /// when the host caps its size.
    pub(crate) fn count(&self) -> u32 {
        self.count
    }

    /// The dynamic slots past the title's (none in sampler heaps).
    pub(crate) fn dynamic_len(&self) -> u32 {
        self.dynamic
    }

    /// The GPU address of the table of element strides by title slot (view
    /// heaps).
    pub(crate) fn strides_gpu(&self) -> Option<u64> {
        self.strides.as_ref().map(|s| s.1)
    }

    /// The table's buffer, for a list to hold.
    pub(crate) fn strides_buffer(&self) -> Option<&ID3D12Resource> {
        self.strides.as_ref().map(|s| &s.0)
    }

    /// Up to `max` title slots whose texture view is due at `frame`, each
    /// with its write number, in service order. Also returns counts for
    /// the whole queue ([`TextureQueue::due_for`]; `room`: whether the card
    /// has room for new textures).
    pub(crate) fn texture_slots_due(&self, frame: u64, max: usize, room: bool) -> (Vec<(u32, u64)>, [u64; 5]) {
        let mut result = Vec::new();
        let mut counts = [0u64; 5];
        self.each_texture_shard(|shard| {
            let (due, counts_shard) = shard.lock().unwrap_or_else(|e| e.into_inner()).due_for(frame, max, room);
            for (k, v) in due {
                if result.len() < max {
                    result.push((k, v));
                }
            }
            for i in 0..5 {
                counts[i] += counts_shard[i];
            }
        });
        result.sort_by_key(|&(_, w)| w);
        result.truncate(max);
        (result, counts)
    }

    /// Every title slot of the queue that is due at `frame`, and whether
    /// it was uploaded before (in part, or whole and waiting for another
    /// look): for the card accounting line
    /// (`crate::resource::card_accounting`).
    pub(crate) fn texture_slots_all_due(&self, frame: u64) -> Vec<(u32, bool)> {
        let mut result = Vec::new();
        self.each_texture_shard(|shard| {
            let mut shard = shard.lock().unwrap_or_else(|e| e.into_inner());
            result.extend(
                shard.slots
                    .iter()
                    .filter(|(_, w)| w.due <= frame)
                    .map(|(&index, w)| (index, w.mapped > 0 || w.checked.2 != 0))
            );
        });
        result
    }

    /// Title slot `index` was brought up to date: due again from frame `again` (uploaded with
    /// that many bytes of its memory there, not all), or not until the
    /// slot is written again.
    pub(crate) fn texture_slot_done(&self, index: u32, write: u64, again: Option<(u64, u64)>) {
        let mut textures = self.textures_shard(index).lock().unwrap_or_else(|e| e.into_inner());
        match again {
            Some((frame, mapped)) => textures.partly(index, write, frame, mapped),
            None => textures.done(index, write),
        }
    }

    /// Title slot `index` was uploaded whole: see [`TextureQueue::recheck`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn texture_slot_recheck(
        &self,
        index: u32,
        write: u64,
        due: u64,
        at: (u64, u64),
        mips: Option<Mips>,
        print: u64,
        looks: u8,
        uploaded: u64,
    ) {
        let mut textures = self.textures_shard(index).lock().unwrap_or_else(|e| e.into_inner());
        textures.recheck(index, write, due, at, mips, print, looks, uploaded);
    }

    /// See [`TextureQueue::checked`].
    pub(crate) fn texture_slot_checked(&self, index: u32) -> Option<((u64, u64, u64, u8, u64), Option<Mips>)> {
        self.textures_shard(index).lock().unwrap_or_else(|e| e.into_inner()).checked(index)
    }

    /// Nothing to upload at title slot `index` now (its memory is not
    /// mapped, or no more of it than before): looked at again after 1, 2,
    /// 4, ... frames, at most `most`.
    pub(crate) fn texture_slot_later(&self, index: u32, write: u64, frame: u64, most: u64) {
        let mut textures = self.textures_shard(index).lock().unwrap_or_else(|e| e.into_inner());
        textures.later(index, write, frame, most);
    }

    /// Title slot `index` was refused: the card has no room for new
    /// textures ([`TextureQueue::full`]).
    pub(crate) fn texture_slot_full(&self, index: u32, write: u64, frame: u64, most: u64) {
        let mut textures = self.textures_shard(index).lock().unwrap_or_else(|e| e.into_inner());
        textures.full(index, write, frame, most);
    }

    /// Bytes of title slot `index`'s memory that were mapped when it was
    /// last uploaded in part (0: not uploaded, or whole).
    pub(crate) fn texture_slot_mapped(&self, index: u32) -> u64 {
        self.textures_shard(index).lock().unwrap_or_else(|e| e.into_inner()).mapped(index)
    }

    /// Takes the ranges of title memory the runtime mapped, unmapped or
    /// wrote since the last call ([`remapped`]) and makes the slots whose
    /// placed texture lies there due from the next frame (by when a write
    /// under way has landed): memory that arrived is uploaded, memory that
    /// went makes the slot null ([`HostHeap::texture_slot_gone`]), and a
    /// texture streamed into the place of another of the same description
    /// is uploaded although no descriptor says so. Slots tables read are
    /// left to their draws. Returns how many slots it made due.
    /// shortcut: the ranges are kept in one place for all heaps; the heap
    /// whose pass runs first takes them (a title has one view heap).
    /// `immediate` (a pass right before a draw of the title's decoration
    /// bake, `crate::draw` `bake_flush`): due in `frame` itself, so memory
    /// that arrived is uploaded before the bake reads it. Ported from fork
    /// PR 1097 a8bcf21d (Azemed), its immediate bake flush.
    pub(crate) fn texture_slots_remapped(&self, frame: u64, immediate: bool) -> usize {
        use std::sync::atomic::Ordering::Relaxed;
        let ranges = std::mem::take(&mut *REMAPPED.lock().unwrap_or_else(|e| e.into_inner()));
        if ranges.is_empty() {
            return 0;
        }
        let mut slots: Vec<u32> = {
            let mut all_slots = Vec::new();
            for placed_shard in &self.placed {
                let placed = placed_shard.lock().unwrap_or_else(|e| e.into_inner());
                for &(start, end) in &ranges {
                    all_slots.extend(placed.slots_in(start, end));
                }
            }
            all_slots
        };
        slots.sort_unstable();
        slots.dedup();
        slots.retain(|&slot| !self.tabled_lately(slot, frame));
        let due = if immediate { frame } else { frame + 1 };
        for &slot in &slots {
            let mut textures = self.textures_shard(slot).lock().unwrap_or_else(|e| e.into_inner());
            textures.changed(slot, frame, due);
        }
        STALE[3].fetch_add(slots.len() as u64, Relaxed);
        slots.len()
    }

    /// Right before a draw of the title's decoration bake: slots uploaded
    /// whole and waiting for their first look again ([`TextureQueue::recheck`])
    /// and slots uploaded with only part of their memory there are due in
    /// `frame`, so a texture whose bytes the title wrote after its upload is
    /// looked at, and uploaded again if changed, before the bake reads it.
    /// How many were made due. Ported from fork PR 1097 a8bcf21d (Azemed).
    pub(crate) fn texture_slots_bake_prepare(&self, frame: u64) -> usize {
        self.textures_shard(0).lock().unwrap_or_else(|e| e.into_inner()).bake_prepare(frame)
    }

    /// Whether host slot `index` is made from the very record the title
    /// has at `handle` now. Its host texture then exists (a slot keeps what
    /// it views), so bringing that texture up to date takes no memory of
    /// the card.
    pub(crate) fn texture_slot_made_from(&self, index: u32, handle: u64) -> bool {
        if index >= self.count {
            return false;
        }
        // SAFETY: the handle is a descriptor slot in title memory.
        let [tag, i, generation] = unsafe { (handle as *const [u32; 3]).read_unaligned() };
        let key = u64::from(generation) << 32 | u64::from(i);
        if tag != VIEW_TAG {
            return false;
        }
        // And the slot holds a host resource: a slot is also noted as made
        // from a view the host could not make (`make` gave nothing, the
        // slot is null and keeps nothing). The host copy of such a
        // texture is held by no slot, may have been let go, and would be
        // made anew here, with the card full. `made` first, then `kept`,
        // as `mirror` takes them.
        let made = self.made_shard(index).lock().unwrap_or_else(|e| e.into_inner());
        made.get(index as usize).is_some_and(|&made| made != 0 && made == key)
            && self.kept.get((index / KEPT_PIECE) as usize).is_some_and(|piece| {
                piece.lock().unwrap_or_else(|e| e.into_inner()).get((index % KEPT_PIECE) as usize).is_some_and(Option::is_some)
            })
    }

    /// The memory of the texture title slot `index` views is not mapped
    /// (any more): a host slot made from it becomes null again, so that
    /// the host copy can be let go (`crate::resource::let_go_idle`; a slot
    /// keeps what it views). True if the slot was made.
    pub(crate) fn texture_slot_gone(&self, index: u32) -> bool {
        let mut made = self.made_shard(index).lock().unwrap_or_else(|e| e.into_inner());
        if made.get(index as usize).is_none_or(|&key| key == 0) {
            return false;
        }
        self.unmake(&mut made, index);
        STALE[4].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        true
    }

    /// Makes host slot `index` null again, as it is before its view is
    /// made: a null view of the kind it held.
    fn unmake(&self, made: &mut [u64], index: u32) {
        let Some(key) = made.get_mut(index as usize).map(std::mem::take) else {
            return;
        };
        self.made_now[index as usize].store(0, std::sync::atomic::Ordering::Release);
        if key == 0 {
            return;
        }
        match view_at(key as u32, (key >> 32) as u32) {
            Some(view) => null_for(&view, self.cpu(index)),
            None => null_view(self.cpu(index)),
        }
        self.keep(index, None);
        // Clear invalid cache - slot is now null (not invalid)
        self.invalid_cache[index as usize].store(0, std::sync::atomic::Ordering::Release);
    }

    /// Title slot `index` is a table's ([`HostHeap::tabled`]). `read`: a
    /// draw read it through the table and brought what it views up to
    /// date, so the slot leaves the queue of textures picked by index; the
    /// caller makes the host slot in place ([`HostHeap::mirror`]), as the
    /// pass would have, for a shader that picks it by index all the same.
    pub(crate) fn texture_slot_tabled(&self, index: u32, read: bool) {
        self.tabled_in(index, read, crate::frame::current());
    }

    /// [`HostHeap::texture_slot_tabled`] in frame `frame`.
    fn tabled_in(&self, index: u32, read: bool, frame: u64) {
        use std::sync::atomic::Ordering::Relaxed;
        let Some(stamp) = self.tabled.get(index as usize) else {
            return;
        };
        let now = (frame as u32).wrapping_add(1) & !TABLED_READ;
        let old = stamp.load(Relaxed);
        if !read {
            if old & !TABLED_READ != now {
                stamp.store(now, Relaxed);
            }
            return;
        }
        // Once per slot and frame: tables are read by every draw.
        if old != now | TABLED_READ {
            stamp.store(now | TABLED_READ, Relaxed);
            if tabled_out() {
                let mut textures = self.textures_shard(index).lock().unwrap_or_else(|e| e.into_inner());
                if textures.slots.remove(&index).is_some() {
                    TABLED_WRITES[1].fetch_add(1, Relaxed);
                }
            }
        }
    }

    /// Whether title slot `index` was a table's within the last
    /// [`TABLED_FRAMES`] frames before `frame` (never, if such slots are
    /// to be queued as any other).
    fn tabled_lately(&self, index: u32, frame: u64) -> bool {
        let stamp = self.tabled_stamp(index) & !TABLED_READ;
        let now = (frame as u32).wrapping_add(1) & !TABLED_READ;
        tabled_out() && stamp != 0 && u64::from(now.wrapping_sub(stamp)) <= TABLED_FRAMES
    }

    /// What `tabled` holds for title slot `index`; 0: never a table's.
    fn tabled_stamp(&self, index: u32) -> u32 {
        self.tabled
            .get(index as usize)
            .map_or(0, |s| s.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// The queue's part of a write of title slot `index` in `frame`:
    /// `texture`, whether it now holds a view of a texture the title may
    /// fill; `seen` as for [`HostHeap::note_buffer`].
    fn queue_texture(&self, index: u32, texture: bool, seen: bool, frame: u64) {
        let stamp = if tabled_out() { self.tabled_stamp(index) } else { 0 };
        let now = (frame as u32).wrapping_add(1) & !TABLED_READ;
        // A table's slot lately: the draw that reads it uploads its
        // texture, so its write waits for that draw.
        let lately = stamp != 0 && u64::from(now.wrapping_sub(stamp & !TABLED_READ)) <= TABLED_FRAMES;
        // Made now by a draw of this frame that read it through a table:
        // up to date.
        let read_now = seen && stamp == now | TABLED_READ;
        let mut textures = self.textures_shard(index).lock().unwrap_or_else(|e| e.into_inner());
        if texture && !read_now {
            let due = if lately && !seen {
                TABLED_WRITES[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                frame + TABLED_GRACE
            } else {
                0
            };
            textures.written(index, frame, due, seen);
        } else {
            textures.remove(index);
        }
    }

    /// The title slots that hold buffer views.
    pub(crate) fn buffer_slots(&self) -> Vec<u32> {
        let mut result = Vec::new();
        self.each_buffer_shard(|shard| {
            result.extend(shard.lock().unwrap_or_else(|e| e.into_inner()).iter().copied());
        });
        result
    }

    /// The title slots whose buffer view was written, or went, since the
    /// last call. Not those the caller's own looks made or found gone
    /// while [`passing`]: a write of the title after such a look marks the
    /// slot as any other (and changes its record, which the pass compares).
    pub(crate) fn buffer_changes(&self) -> Vec<u32> {
        let mut changed = self.changed.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *changed).into_iter().collect()
    }

    /// Records what title slot `index` now holds for shaders that pick
    /// buffers from the heap: its element stride in the table, and whether
    /// it is a buffer view at all.
    /// `seen`: the slot was not written now, a draw or a pass made its
    /// host slot from what it held.
    /// Returns the title slots whose placed texture view the write made
    /// stale ([`Spans`]): out of the queue of textures already, their host
    /// slots are the caller's to make null (it holds `made`).
    fn note_buffer(&self, index: u32, view: Option<&View>, seen: bool) -> Vec<u32> {
        if self.sampler {
            return Vec::new();
        }
        // Fast path: if view is None or doesn't indicate texture, skip heavy work
        if view.is_none() {
            // Still need to queue_texture for texture tracking
            self.queue_texture(index, false, seen, crate::frame::current());
            // Update strides if needed
            if let Some((_, _, data)) = &self.strides
                && index < victory_shader::emit::SLOT_STRIDES_COUNT
            {
                unsafe { (*data as *mut u32).add(index as usize).write_volatile(0) };
            }
            // Update buffers (remove if no stride)
            let mut buffers = self.buffers_shard(index).lock().unwrap_or_else(|e| e.into_inner());
            buffers.remove(&index);
            return Vec::new();
        }

        // Measured for the texture side of the same gap (textures a shader
        // picks by a computed index are neither made, if placed, nor
        // uploaded): slots written with a placed texture view, and with a
        // view of a texture object.
        let texture = match view {
            Some(View::Placed(false, resource, _)) if resource[0] != 1 => {
                PLACED_TEXTURE_SLOTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                true
            }
            Some(View::Shader(r, _)) if crate::resource::buffer_bytes(*r as crate::object::This).is_none() => {
                OBJECT_TEXTURE_SLOTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                true
            }
            _ => false,
        };
        self.queue_texture(index, texture, seen, crate::frame::current());
        // The memory a placed texture view describes. Not for a slot
        // tables read (thousands of writes a frame, each read by its
        // draw): no pass looks at it, and the same texture's view in a
        // slot shaders pick by index says what it says.
        let placed_view = match view {
            Some(View::Placed(false, resource, desc))
                if resource[0] != 1 && !self.tabled_lately(index, crate::frame::current()) =>
            {
                Some((resource, u64::from(desc[10]) | u64::from(desc[11]) << 32))
            }
            _ => None,
        };
        let stale = {
            use std::sync::atomic::Ordering::Relaxed;
            let mut placed = self.placed_shard(index).lock().unwrap_or_else(|e| e.into_inner());
            let span = placed_view
                .and_then(|(resource, start)| Some((start, start.saturating_add(placed.bytes(resource)?))));
            match span {
                Some((start, end)) => {
                    SPANNED[0].fetch_min(start, Relaxed);
                    SPANNED[1].fetch_max(end, Relaxed);
                    placed.rule = stale_rule();
                    placed.note(index, start, end, !seen)
                }
                None => {
                    placed.forget(index);
                    Vec::new()
                }
            }
        };
        if !stale.is_empty() {
            STALE[0].fetch_add(stale.len() as u64, std::sync::atomic::Ordering::Relaxed);
            let mut textures = self.textures_shard(index).lock().unwrap_or_else(|e| e.into_inner());
            for &slot in &stale {
                textures.remove(slot);
            }
        }
        let stride = view.and_then(view_stride);
        if let Some((_, _, data)) = &self.strides
            && index < victory_shader::emit::SLOT_STRIDES_COUNT
        {
            // SAFETY: inside the mapping; shaders read it while we write (a
            // dword, either value is one the slot had).
            unsafe { (*data as *mut u32).add(index as usize).write_volatile(stride.unwrap_or(0)) };
        }
        let mut buffers = self.buffers_shard(index).lock().unwrap_or_else(|e| e.into_inner());
        let was = if stride.is_some() {
            buffers.insert(index);
            true
        } else {
            buffers.remove(&index)
        };
        // Not when the pass over the heap's buffer views makes the slot
        // itself ([`passing`]): it has the slot in hand, and a mark here
        // had it look at the slot again next pass, for nothing (every
        // written slot was looked at in two passes).
        if was && !PASSING.with(std::cell::Cell::get) {
            self.changed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(index);
        }
        Vec::new()
    }

    #[cfg(test)]
    pub(crate) fn release(&self, first: u32, n: u32) {
        let mut ring = self.next.lock().unwrap_or_else(|e| e.into_inner());
        let _given = self.release_locked(&mut ring, first, n);
        #[cfg(feature = "diagnostics")]
        crate::copystat::dynamic_returned(_given);
    }

    pub(crate) fn retire_ranges(
        &self,
        fence: u64,
        slots: &[(std::sync::Arc<Self>, u32, u32)],
        completed: Option<u64>,
    ) {
        let mut ring = self.next.lock().unwrap_or_else(|e| e.into_inner());
        if ring.retiring.back().is_none_or(|p| p.0 != fence) {
            ring.retiring.push_back((fence, Vec::new()));
        }
        ring.retiring.back_mut().unwrap().1.extend(slots.iter().map(|&(_, first, n)| (first, n)));
        if let Some(completed) = completed {
            let _given = self.release_completed_locked(&mut ring, completed);
            #[cfg(feature = "diagnostics")]
            if _given > 0 {
                crate::copystat::add(crate::copystat::C::DynamicPollSlots, u64::from(_given));
            }
        }
    }

    pub(crate) fn release_completed(&self, fence: u64) {
        let mut ring = self.next.lock().unwrap_or_else(|e| e.into_inner());
        self.release_completed_locked(&mut ring, fence);
    }

    fn release_completed_locked(&self, ring: &mut Ring, fence: u64) -> u32 {
        let mut total = 0;
        while let Some(ranges) = ring.completed_ranges(fence) {
            #[cfg(feature = "diagnostics")]
            crate::copystat::add(crate::copystat::C::DynamicEarly, ranges.len() as u64);
            static JOIN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            let give = |(first, n)| self.release_locked(ring, first, n);
            let join = *JOIN.get_or_init(|| {
                std::env::var_os("VICTORY_DYNAMIC_COALESCE").is_none_or(|v| v != "0")
            });
            let given: u32 = if join {
                coalesced(ranges).map(give).sum()
            } else {
                ranges.into_iter().map(give).sum()
            };
            #[cfg(feature = "diagnostics")]
            crate::copystat::dynamic_returned(given);
            total += given;
        }
        total
    }

    fn release_locked(&self, ring: &mut Ring, first: u32, n: u32) -> u32 {
        let Some(at) = first.checked_sub(self.count) else {
            return 0;
        };
        let end = at.saturating_add(n).min(self.dynamic) as usize;
        if let Some((given, run)) = ring.release(at as usize, end) {
            self.free.fetch_add(given, std::sync::atomic::Ordering::Relaxed);
            // Only the run the given slots now lie in can be longer than
            // before: when it is shorter, tables of `too_long` slots or more
            // still have no room and are refused without the search.
            let too_long = &self.too_long;
            if run >= too_long.load(std::sync::atomic::Ordering::Relaxed) as usize {
                too_long.store(u32::MAX, std::sync::atomic::Ordering::Relaxed);
            }
            return given;
        }
        0
    }

    pub(crate) fn release_ranges(&self, slots: &[(std::sync::Arc<Self>, u32, u32)]) {
        let mut ring = self.next.lock().unwrap_or_else(|e| e.into_inner());
        let _given: u32 = slots.iter().map(|(_, first, n)| self.release_locked(&mut ring, *first, *n)).sum();
        #[cfg(feature = "diagnostics")]
        crate::copystat::dynamic_returned(_given);
    }

    /// Makes slot `index` (a dynamic one) from `view`; a null view if the
    /// host cannot.
    pub(crate) fn make_at(&self, index: u32, view: &View) {
        let kept = make(view, self.cpu(index));
        if kept.is_none() {
            null_for(view, self.cpu(index));
        }
        self.keep(index, kept.flatten());
    }

    /// Makes slot `index` read nothing: a null view, or a plain sampler.
    pub(crate) fn make_null(&self, index: u32) {
        if self.sampler {
            self.null_sampler(index);
        } else {
            null_view(self.cpu(index));
        }
        self.keep(index, None);
        // Clear invalid cache - slot is now null (not invalid)
        self.invalid_cache[index as usize].store(0, std::sync::atomic::Ordering::Release);
    }

    /// Slot `index` now views `resource` (or nothing of the host's).
    fn keep(&self, index: u32, resource: Option<ID3D12Resource>) {
        let Some(piece) = self.kept.get((index / KEPT_PIECE) as usize) else {
            return;
        };
        let old = {
            let mut kept = piece.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::replace(&mut kept[(index % KEPT_PIECE) as usize], resource)
        };
        // A running submission may still read the old resource through
        // this slot. (Outside the lock: letting go takes another, or frees
        // a host resource.)
        if let Some(old) = old {
            crate::host::retire(old);
        }
    }

    /// Makes slot `index` (a dynamic one) a raw buffer view of `len` bytes
    /// of `buffer` from byte `offset` (a multiple of 16): a root SRV with a
    /// size, whose reads past it return zeros instead of reaching whatever
    /// the host placed after the buffer.
    pub(crate) fn raw_at(&self, index: u32, buffer: &ID3D12Resource, offset: u64, len: u64) {
        let Some(host) = host() else { return };
        let elements = (len / 4).clamp(1, u64::from(u32::MAX)) as u32;
        let desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: DXGI_FORMAT_R32_TYPELESS,
            ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
            Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                Buffer: D3D12_BUFFER_SRV {
                    FirstElement: offset / 4,
                    NumElements: elements,
                    StructureByteStride: 0,
                    Flags: D3D12_BUFFER_SRV_FLAG_RAW,
                },
            },
        };
        // SAFETY: a slot of our heap and a live buffer covering the range.
        unsafe {
            host.device
                .CreateShaderResourceView(buffer, Some(&desc), self.cpu(index))
        };
        self.keep(index, Some(buffer.clone()));
    }

    /// Makes slot `index` (a dynamic one) a raw buffer view that reads
    /// zeros.
    pub(crate) fn null_raw_at(&self, index: u32) {
        null_buffer_view(self.cpu(index));
        self.keep(index, None);
    }

    /// Makes slot `index` (a dynamic one) a raw unordered access view of
    /// `len` bytes of `buffer` from byte `offset` (a multiple of 16): a
    /// root UAV with a size, whose accesses past it do nothing instead of
    /// reaching whatever the host placed after the buffer.
    pub(crate) fn raw_uav_at(&self, index: u32, buffer: &ID3D12Resource, offset: u64, len: u64) {
        let Some(host) = host() else { return };
        let elements = (len / 4).clamp(1, u64::from(u32::MAX)) as u32;
        let desc = D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT_R32_TYPELESS,
            ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
            Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                Buffer: D3D12_BUFFER_UAV {
                    FirstElement: offset / 4,
                    NumElements: elements,
                    StructureByteStride: 0,
                    CounterOffsetInBytes: 0,
                    Flags: D3D12_BUFFER_UAV_FLAG_RAW,
                },
            },
        };
        // SAFETY: a slot of our heap and a live buffer covering the range.
        unsafe {
            host.device.CreateUnorderedAccessView(
                buffer,
                None::<&ID3D12Resource>,
                Some(&desc),
                self.cpu(index),
            )
        };
        self.keep(index, Some(buffer.clone()));
    }

    /// Makes slot `index` (a dynamic one) a raw unordered access view of
    /// nothing: reads give zeros, writes are dropped.
    pub(crate) fn null_raw_uav_at(&self, index: u32) {
        self.keep(index, None);
        let Some(host) = host() else { return };
        let desc = D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT_R32_TYPELESS,
            ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
            Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                Buffer: D3D12_BUFFER_UAV {
                    NumElements: 1,
                    Flags: D3D12_BUFFER_UAV_FLAG_RAW,
                    ..Default::default()
                },
            },
        };
        // SAFETY: a slot of our heap; a null resource is allowed with a desc.
        unsafe {
            host.device.CreateUnorderedAccessView(
                None::<&ID3D12Resource>,
                None::<&ID3D12Resource>,
                Some(&desc),
                self.cpu(index),
            )
        };
    }

    /// The shader-visible handle of slot `index`.
    pub(crate) fn gpu_handle(&self, index: u32) -> D3D12_GPU_DESCRIPTOR_HANDLE {
        // SAFETY: a live shader-visible heap.
        let start = unsafe { self.heap.GetGPUDescriptorHandleForHeapStart().ptr };
        D3D12_GPU_DESCRIPTOR_HANDLE {
            ptr: start + u64::from(index) * u64::from(self.step),
        }
    }

    /// Makes slot `index` (a dynamic one) a constant buffer view of host
    /// memory (the list's own, which it holds).
    /// (Unused since table constants are raw views, `raw_constant_at`;
    /// kept for a way back.)
    #[allow(dead_code)]
    pub(crate) fn constant_at(&self, index: u32, gpu: u64, size: u32) {
        self.keep(index, None);
        let Some(host) = host() else { return };
        let desc = D3D12_CONSTANT_BUFFER_VIEW_DESC {
            BufferLocation: gpu,
            SizeInBytes: constant_view_size(size),
        };
        // SAFETY: a slot of our heap; the memory is the caller's.
        unsafe {
            host.device
                .CreateConstantBufferView(Some(&desc), self.cpu(index))
        };
    }

    /// Makes slot `index` (a dynamic one) a raw view of `len` bytes of
    /// `buffer` from byte `offset` (the list's own upload memory, which it
    /// holds): a table's constant buffer copied for the draw, read as
    /// translated shaders read table constants. A null raw view if the
    /// host would refuse it.
    pub(crate) fn raw_constant_at(&self, index: u32, buffer: &ID3D12Resource, offset: u64, len: u64) {
        self.keep(index, None);
        if raw_srv(buffer, offset, len.next_multiple_of(4), self.cpu(index)).is_none() {
            null_buffer_view(self.cpu(index));
        }
    }

    /// Makes slot `index` a null raw buffer view (a constant buffer's
    /// slot that reads nothing).
    pub(crate) fn make_null_buffer(&self, index: u32) {
        null_buffer_view(self.cpu(index));
        self.keep(index, None);
    }

    fn cpu(&self, index: u32) -> D3D12_CPU_DESCRIPTOR_HANDLE {
        D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: self.cpu + index as usize * self.step as usize,
        }
    }

    /// Makes host slot `index` from the record the title has at `handle`
    /// if it changed since, and returns the view recorded there. Buffer
    /// views are made only for draws (`for_draw`), which also copy their
    /// data: a host copy of a buffer is made on first use.
    pub(crate) fn mirror(&self, index: u32, handle: u64, for_draw: bool) -> Option<View> {
        #[cfg(feature = "diagnostics")]
        {
            if !self.sampler {
                crate::copystat::title_slot(index);
            }
        }
        if index >= self.count {
            // Diagnostic (NVIDIA heap cap): the highest title slot past the
            // host heap seen so far.
            static HIGHEST: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if HIGHEST.fetch_max(index, std::sync::atomic::Ordering::Relaxed) < index {
                tracing::warn!(
                    index,
                    count = self.count,
                    for_draw,
                    "title slot past the host heap"
                );
            }
            return None;
        }
        // SAFETY: the handle is a descriptor slot in title memory.
        let [tag, i, generation] = unsafe { (handle as *const [u32; 3]).read_unaligned() };
        // Generations start at 1, so a made slot's key is never 0.
        let key = u64::from(generation) << 32 | u64::from(i);

        // Fast path: check invalid cache first (Bit 0 = invalid, Bit 1 = checked this frame)
        let cache = self.invalid_cache[index as usize].load(std::sync::atomic::Ordering::Acquire);
        if cache & 1 != 0 && tag == VIEW_TAG {
            // Known invalid slot, skip VIEWS RwLock and note_buffer
            mirror_path(3); // invalid record
            return None;
        }

        let view = mirror_measure(0, || (tag == VIEW_TAG).then(|| view_at(i, generation)).flatten());
        // Made from this record already: nothing to do, and no lock.
        if view.is_some() && heap_locks() && self.made_now[index as usize].load(std::sync::atomic::Ordering::Acquire) == key {
            // Fast path: return cached View if available, avoiding VIEWS RwLock
            if let Some(cached) = self.made_view.lock().unwrap_or_else(|e| e.into_inner())[index as usize].as_ref() {
                return Some(cached.clone());
            }
            mirror_path(1); // locked unchanged
            return view;
        }
        let mut made = mirror_measure(1, || self.made_shard(index).lock().unwrap_or_else(|e| e.into_inner()));
        let Some(view) = view else {
            // Not a view of ours (never written, or of a resource the
            // title released): nothing to read.
            mirror_path(3); // invalid record
            // Cache invalid state (tag == VIEW_TAG but view_at returned None)
            if tag == VIEW_TAG {
                self.invalid_cache[index as usize].store(1, std::sync::atomic::Ordering::Release);
            }
            mirror_measure(2, || {
                if made[index as usize] != 0 {
                    made[index as usize] = 0;
                    self.made_now[index as usize].store(0, std::sync::atomic::Ordering::Release);
                    self.make_null(index);
                    let _ = self.note_buffer(index, None, false);
                } else if for_draw {
                    // Left for later and stale since: out of the sets and the stride table.
                    let _ = self.note_buffer(index, None, false);
                }
            });
            return None;
        };
        if made[index as usize] != key {
            // Views of other textures in the memory this one describes
            // are stale: their host slots read nothing from here on, and
            // let go of the host copies they held.
            mirror_measure(2, || {
                for stale in self.note_buffer(index, Some(&view), for_draw) {
                    if stale != index {
                        self.unmake(&mut made, stale);
                    }
                }
            });
            // A view of a texture object that has no host copy yet waits
            // for it, as buffer views wait for a draw: the slot is made
            // when the texture is first used (`super::make_waiting`), or
            // here when a draw reads the slot through a table.
            let waiting = if for_draw { None } else { view.waiting_texture() };
            if let Some(texture) = waiting {
                super::wait_for(texture, handle);
            }
            let later = !for_draw && (view.is_buffer() || waiting.is_some());
            // Another view of the placed texture the slot shows already (a
            // title that streams a texture in writes its view again with
            // more mips each time some arrive): the slot goes on showing
            // the old view until a draw or the texture pass makes the new
            // one and uploads what it shows. Made null here, the texture
            // read as nothing for as long as it waited in the queue, every
            // time it got sharper.
            if later
                && keep_views()
                && view_at(made[index as usize] as u32, (made[index as usize] >> 32) as u32)
                    .is_some_and(|old| same_placed_texture(&old, &view))
            {
                KEPT_VIEWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return Some(view);
            }
            made[index as usize] = if later { 0 } else { key };
            let kept = mirror_measure(3, || if later {
                None
            } else {
                make(&view, self.cpu(index))
            });
            if kept.is_none() && !self.sampler {
                mirror_measure(5, || null_for(&view, self.cpu(index)));
            }
            mirror_measure(4, || self.keep(index, kept.flatten()));
            // After the slot is made: a reader without the lock takes the
            // record as made only once it is.
            self.made_now[index as usize].store(made[index as usize], std::sync::atomic::Ordering::Release);
            // Cache the View for future fast-path hits (avoids VIEWS RwLock + view_at)
            self.made_view.lock().unwrap_or_else(|e| e.into_inner())[index as usize] = Some(view.clone());
            // Clear invalid cache - slot is now valid
            self.invalid_cache[index as usize].store(0, std::sync::atomic::Ordering::Release);
            mirror_path(2); // rebuilt view
        }
        Some(view)
    }

    /// Like `mirror` but returns only whether the slot was made/updated,
    /// avoiding `View` allocation for callers that only need success status.
    pub(crate) fn mirror_no_view(&self, index: u32, handle: u64, for_draw: bool) -> bool {
        #[cfg(feature = "diagnostics")]
        {
            if !self.sampler {
                crate::copystat::title_slot(index);
            }
        }
        if index >= self.count {
            static HIGHEST: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if HIGHEST.fetch_max(index, std::sync::atomic::Ordering::Relaxed) < index {
                tracing::warn!(
                    index,
                    count = self.count,
                    for_draw,
                    "title slot past the host heap"
                );
            }
            return false;
        }
        // SAFETY: the handle is a descriptor slot in title memory.
        let [tag, i, generation] = unsafe { (handle as *const [u32; 3]).read_unaligned() };
        let key = u64::from(generation) << 32 | u64::from(i);

        let view = mirror_measure(0, || (tag == VIEW_TAG).then(|| view_at(i, generation)).flatten());
        if view.is_some() && heap_locks() && self.made_now[index as usize].load(std::sync::atomic::Ordering::Acquire) == key {
            mirror_path(1); // locked unchanged
            return true;
        }
        let mut made = mirror_measure(1, || self.made_shard(index).lock().unwrap_or_else(|e| e.into_inner()));
        let Some(view) = view else {
            mirror_path(3); // invalid record
            mirror_measure(2, || {
                if made[index as usize] != 0 {
                    made[index as usize] = 0;
                    self.made_now[index as usize].store(0, std::sync::atomic::Ordering::Release);
                    self.make_null(index);
                    let _ = self.note_buffer(index, None, false);
                } else if for_draw {
                    let _ = self.note_buffer(index, None, false);
                }
            });
            return false;
        };
        if made[index as usize] != key {
            mirror_measure(2, || {
                for stale in self.note_buffer(index, Some(&view), for_draw) {
                    if stale != index {
                        self.unmake(&mut made, stale);
                    }
                }
            });
            let waiting = if for_draw { None } else { view.waiting_texture() };
            if let Some(texture) = waiting {
                super::wait_for(texture, handle);
            }
            let later = !for_draw && (view.is_buffer() || waiting.is_some());
            if later
                && keep_views()
                && view_at(made[index as usize] as u32, (made[index as usize] >> 32) as u32)
                    .is_some_and(|old| same_placed_texture(&old, &view))
            {
                KEPT_VIEWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return true;
            }
            made[index as usize] = if later { 0 } else { key };
            let kept = mirror_measure(3, || if later {
                None
            } else {
                make(&view, self.cpu(index))
            });
            if kept.is_none() && !self.sampler {
                mirror_measure(5, || null_for(&view, self.cpu(index)));
            }
            mirror_measure(4, || self.keep(index, kept.flatten()));
            self.made_now[index as usize].store(made[index as usize], std::sync::atomic::Ordering::Release);
            mirror_path(2); // rebuilt view
        }
        true
    }

    /// Whether host slot `index` shows `buffer`; `None` if it shows no
    /// resource (diagnostics: a buffer view's slot must show the host copy
    /// that is brought up to date for it).
    #[cfg(feature = "diagnostics")]
    pub(crate) fn shows(&self, index: u32, buffer: &ID3D12Resource) -> Option<bool> {
        let kept = self.kept.get((index / KEPT_PIECE) as usize)?.lock().unwrap_or_else(|e| e.into_inner());
        let shown = kept.get((index % KEPT_PIECE) as usize)?.as_ref()?;
        Some(windows::core::Interface::as_raw(shown) == windows::core::Interface::as_raw(buffer))
    }

    /// What the queue of textures picked by index knows of title slot
    /// `index` at `frame`, in a few words (diagnostics).
    #[cfg(feature = "diagnostics")]
    pub(crate) fn texture_slot_state(&self, index: u32, frame: u64) -> String {
        let textures = self.textures_shard(index).lock().unwrap_or_else(|e| e.into_inner());
        match textures.slots.get(&index) {
            None => "not queued".into(),
            Some(w) => format!(
                "queued due={} written@{} looks={} part_mapped={} recheck={}",
                if w.due <= frame { "now".to_string() } else { format!("@{}", w.due) },
                w.written,
                w.looks,
                w.mapped,
                w.checked.2 != 0
            ),
        }
    }

    /// The view host slot `index` was made from, in a few words, with the
    /// title and host addresses of a buffer view (diagnostics). "null" for
    /// a slot never made or left null.
    #[cfg(feature = "diagnostics")]
    pub(crate) fn describe_slot(&self, index: u32) -> Option<String> {
        let key = *self.made_shard(index).lock().unwrap_or_else(|e| e.into_inner()).get(index as usize)?;
        if key == 0 {
            return Some("null".into());
        }
        let view = view_at(key as u32, (key >> 32) as u32)?;
        let mut text: String = format!("{view:?}").chars().take(120).collect();
        if let Some((address, len)) = buffer_range(&view) {
            let host = crate::shadow::find(address, len.max(1)).map(|s| s.gpu(address));
            text.push_str(&format!(" title {address:#x}+{len:#x} host {host:#x?}"));
        }
        Some(text)
    }
}

/// Logs `what` the first time it happens.
macro_rules! once {
    ($($arg:tt)*) => {{
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| tracing::warn!($($arg)*));
    }};
}

/// A texture view `desc` on a host copy that leaves out the title's
/// `drop` largest levels (`crate::resource::drop_levels`): its levels
/// shift down by as many ([`crate::resource::shift_levels`]). 2D textures,
/// arrays and cubes only, the dimensions such copies have.
fn shift_view_levels(drop: u32, desc: &mut D3D12_SHADER_RESOURCE_VIEW_DESC) {
    if drop == 0 {
        return;
    }
    let shift = crate::resource::shift_levels;
    // SAFETY: each arm reads and writes the union member its dimension names.
    unsafe {
        match desc.ViewDimension {
            D3D12_SRV_DIMENSION_TEXTURE2D => {
                let t = &mut desc.Anonymous.Texture2D;
                shift(drop, &mut t.MostDetailedMip, &mut t.MipLevels, &mut t.ResourceMinLODClamp);
            }
            D3D12_SRV_DIMENSION_TEXTURE2DARRAY => {
                let t = &mut desc.Anonymous.Texture2DArray;
                shift(drop, &mut t.MostDetailedMip, &mut t.MipLevels, &mut t.ResourceMinLODClamp);
            }
            D3D12_SRV_DIMENSION_TEXTURECUBE => {
                let t = &mut desc.Anonymous.TextureCube;
                shift(drop, &mut t.MostDetailedMip, &mut t.MipLevels, &mut t.ResourceMinLODClamp);
            }
            D3D12_SRV_DIMENSION_TEXTURECUBEARRAY => {
                let t = &mut desc.Anonymous.TextureCubeArray;
                shift(drop, &mut t.MostDetailedMip, &mut t.MipLevels, &mut t.ResourceMinLODClamp);
            }
            _ => {}
        }
    }
}

/// A view description from its words, with the format as the host has it.
fn srv_desc(words: &[u32; 10]) -> D3D12_SHADER_RESOURCE_VIEW_DESC {
    let mut words = *words;
    words[0] = crate::resource::pc_format(words[0]);
    // SAFETY: 40 bytes in the host's layout.
    unsafe {
        std::ptr::from_ref(&words)
            .cast::<D3D12_SHADER_RESOURCE_VIEW_DESC>()
            .read_unaligned()
    }
}

/// The unsigned integer format with the layout of a 16-bit float or
/// signed integer format (one, two or four channels), if `format` is one.
pub(crate) fn half_format(format: u32) -> Option<u32> {
    match format {
        // R16G16B16A16_FLOAT and _SINT: R16G16B16A16_UINT.
        10 | 14 => Some(12),
        // R16G16_FLOAT and _SINT: R16G16_UINT.
        34 | 38 => Some(36),
        // R16_FLOAT and _SINT: R16_UINT.
        54 | 59 => Some(57),
        _ => None,
    }
}

/// `view` as a shader is shown it where it moves a buffer's 16-bit
/// channels as unsigned integers
/// (`victory_shader::emit::HostShader::half_views`): a buffer view in a
/// 16-bit float or signed format, in the unsigned format of the same
/// layout, through which every 16-bit pattern is read and stored as it
/// is. `None` for every other view: the title's own serves (an unsigned
/// one already carries the bits; a texture can only be viewed in the
/// formats it was made for).
pub(crate) fn half_view(view: &View) -> Option<View> {
    // Buffer views: dimension 1 in the title's description of either kind.
    const BUFFER: u32 = 1;
    match view {
        View::Shader(resource, Some(desc)) if desc[1] == BUFFER => {
            let mut desc = *desc;
            desc[0] = half_format(desc[0])?;
            Some(View::Shader(*resource, Some(desc)))
        }
        View::Unordered(resource, counter, Some(desc)) if desc[1] == BUFFER => {
            let mut desc = *desc;
            desc[0] = half_format(desc[0])?;
            Some(View::Unordered(*resource, *counter, Some(desc)))
        }
        View::Placed(unordered, resource, desc) if resource[0] == BUFFER && desc[1] == BUFFER => {
            let mut desc = *desc;
            desc[0] = half_format(desc[0])?;
            Some(View::Placed(*unordered, *resource, desc))
        }
        _ => None,
    }
}

/// The format of `view`, a buffer view through which a shader moves
/// 16-bit channels as unsigned integers
/// (`victory_shader::emit::HostShader::half_views`), if those integers
/// are not what the console gives: a format of wider channels, a
/// normalised one or a signed 8-bit one, whose values the console converts
/// to and from halves. `None` for the formats the integers serve exactly
/// (16 bits a channel of any kind, through [`half_view`] where needed, and
/// unsigned channels of fewer bits; the title's are 16-bit unsigned) and
/// for any other view.
pub(crate) fn half_inexact(view: &View) -> Option<u32> {
    // Buffer views: dimension 1 in the title's description of either kind.
    const BUFFER: u32 = 1;
    // R16G16B16A16, R16G16 and R16 (float, unsigned, signed), then
    // R10G10B10A2, R8G8B8A8, R8G8 and R8 unsigned.
    const EXACT: [u32; 13] = [10, 12, 14, 34, 36, 38, 54, 57, 59, 25, 30, 50, 62];
    let format = match view {
        View::Shader(_, Some(desc)) | View::Unordered(_, _, Some(desc)) if desc[1] == BUFFER => {
            desc[0]
        }
        View::Placed(_, resource, desc) if resource[0] == BUFFER && desc[1] == BUFFER => desc[0],
        _ => return None,
    };
    (!EXACT.contains(&format)).then_some(format)
}

fn uav_desc(words: &[u32; 10]) -> D3D12_UNORDERED_ACCESS_VIEW_DESC {
    let mut words = *words;
    words[0] = crate::resource::pc_format(words[0]);
    // SAFETY: 40 bytes in the host's layout.
    unsafe {
        std::ptr::from_ref(&words)
            .cast::<D3D12_UNORDERED_ACCESS_VIEW_DESC>()
            .read_unaligned()
    }
}

/// A buffer view of title memory at `address` on its host copy: the first
/// element moves by the copy's offset.
/// Bytes per element of a buffer view.
fn element_size(desc: &D3D12_SHADER_RESOURCE_VIEW_DESC) -> u64 {
    // SAFETY: a buffer view's union member.
    let buffer = unsafe { desc.Anonymous.Buffer };
    if buffer.StructureByteStride > 0 {
        u64::from(buffer.StructureByteStride)
    } else if buffer.Flags.0 & D3D12_BUFFER_SRV_FLAG_RAW.0 != 0 {
        4
    } else {
        crate::resource::element(desc.Format.0 as u32).map_or(0, |(bytes, _)| bytes)
    }
}

/// The title memory a buffer shader or unordered access view reads: its
/// address and length.
pub(crate) fn buffer_range(view: &View) -> Option<(u64, u64)> {
    let (base, words): (u64, &[u32; 10]) = match view {
        View::Shader(r, Some(desc)) => (
            crate::resource::buffer_bytes(*r as crate::object::This)?.0 as u64,
            desc,
        ),
        View::Placed(false, resource, desc) if resource[0] == 1 => (
            u64::from(desc[10]) | u64::from(desc[11]) << 32,
            desc[..10].try_into().ok()?,
        ),
        View::Unordered(r, _, Some(desc)) => {
            let base = crate::resource::buffer_bytes(*r as crate::object::This)?.0 as u64;
            return unordered_range(base, desc);
        }
        View::Placed(true, resource, desc) if resource[0] == 1 => {
            let base = u64::from(desc[10]) | u64::from(desc[11]) << 32;
            return unordered_range(base, desc[..10].try_into().ok()?);
        }
        _ => return None,
    };
    let desc = srv_desc(words);
    let element = element_size(&desc);
    // SAFETY: a buffer view's union member.
    let buffer = unsafe { desc.Anonymous.Buffer };
    Some((
        base + buffer.FirstElement * element,
        u64::from(buffer.NumElements) * element,
    ))
}

/// The title memory the host slot of buffer view `view` is made on
/// (address, bytes): the whole buffer the view is of, which is what
/// [`make`] finds the host copy by. [`buffer_range`] is the part of it the
/// view reads.
pub(crate) fn buffer_extent(view: &View) -> Option<(u64, u64)> {
    match view {
        View::Shader(r, Some(_)) | View::Unordered(r, _, Some(_)) => {
            let (data, len) = crate::resource::buffer_bytes(*r as crate::object::This)?;
            Some((data as u64, len))
        }
        View::Placed(_, resource, desc) if resource[0] == 1 => Some((
            u64::from(desc[10]) | u64::from(desc[11]) << 32,
            u64::from(resource[4]) | u64::from(resource[5]) << 32,
        )),
        _ => None,
    }
}

/// Whether a buffer view is brought up to date in the host copy its slot
/// shows ([`buffer_copy`]). On by default; `VICTORY_VIEW_EXTENT=0` looks the
/// copy up by the view's range alone, as before.
fn view_extent() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_VIEW_EXTENT").is_none_or(|v| v != "0"))
}

/// The host copy buffer view `view` reads, and the title range (address,
/// bytes) it reads there; `None` for other views and empty ranges.
///
/// The copy is the one the view's slot shows: [`make`] finds it by the
/// whole buffer the view is of ([`buffer_extent`]). Looked up by the range
/// the view reads, as it was, the copy is another one wherever the two
/// lookups differ: for memory outside buffer objects, a view of a part of a
/// buffer that crosses a window line (the buffer's window is two cells, the
/// part's one), and for buffer objects, a part that also lies in a smaller
/// buffer object. The slot then showed a copy nothing brought up to date,
/// and what shaders wrote through it was looked for in the other copy.
pub(crate) fn buffer_copy(view: &View) -> Option<(std::sync::Arc<crate::shadow::Shadow>, u64, u64)> {
    // A constant buffer: the copy its raw view shows (`constant_copy`).
    if let View::Constant(address, size) = *view {
        return constant_copy(address, size).map(|(shadow, len)| (shadow, address, len));
    }
    let (address, len) = buffer_range(view)?;
    if len == 0 {
        return None;
    }
    if view_extent()
        && let Some((base, width)) = buffer_extent(view)
        // (The console is lax: a view may begin past its buffer's end.)
        && address >= base
        && address - base < width.max(1)
        && let Some(shadow) = crate::shadow::find(base, width)
    {
        return Some((shadow, address, len));
    }
    crate::shadow::find(address, len).map(|shadow| (shadow, address, len))
}

/// [`buffer_range`] of an unordered access view of memory at `base`.
fn unordered_range(base: u64, words: &[u32; 10]) -> Option<(u64, u64)> {
    let desc = uav_desc(words);
    // SAFETY: a buffer view's union member.
    let buffer = unsafe { desc.Anonymous.Buffer };
    let element = if buffer.StructureByteStride > 0 {
        u64::from(buffer.StructureByteStride)
    } else if buffer.Flags.0 & D3D12_BUFFER_UAV_FLAG_RAW.0 != 0 {
        4
    } else {
        crate::resource::element(desc.Format.0 as u32)?.0
    };
    Some((
        base + buffer.FirstElement * element,
        u64::from(buffer.NumElements) * element,
    ))
}

/// The host resource a slot made by [`make`] views, if any.
type Kept = Option<ID3D12Resource>;

/// A structured buffer view (`first`, `count` elements of `stride` bytes,
/// `offset` bytes into its host copy) as a raw view of the same bytes:
/// its first dword and its dwords. Translated shaders read untyped buffers
/// as raw bytes at `element * stride + offset` (the stride from the
/// used-resource entry, or from the table by heap slot for a buffer picked
/// by index), and a raw read through a structured view gives zeros on
/// NVIDIA (reported upstream with PR #193; structured reads through raw
/// views were right there). So the host's view is raw whenever it can be:
/// the bytes must start on 16 (the host's raw view alignment) and be whole
/// dwords. A view that cannot stays structured.
fn raw_window(offset: u64, first: u64, count: u32, stride: u32) -> Option<(u64, u32)> {
    if stride == 0 {
        return None;
    }
    let start = first.checked_mul(u64::from(stride))?.checked_add(offset)?;
    let bytes = u64::from(count) * u64::from(stride);
    (start % 16 == 0 && bytes % 4 == 0)
        .then(|| u32::try_from(bytes / 4).ok().map(|dwords| (start / 4, dwords)))
        .flatten()
}

/// Whether the host accepts a buffer view with these fields of a buffer of
/// `width` bytes. It removes the device for one it does not (view creation
/// returns nothing), and the console is laxer: the title's heap holds
/// views past their memory's end, of no elements, of typeless formats
/// without the raw flag, with strides the host has no structured buffers
/// for. Since every buffer view of the heap is made for shaders that pick
/// them by index, one such view ended the run (2026-11-24, run 10).
fn buffer_view_ok(format: DXGI_FORMAT, first: u64, count: u32, stride: u32, raw: bool, width: u64) -> bool {
    use crate::resource::family;
    let f = format.0 as u32;
    let element = if raw {
        // raw: R32 typeless, counted in dwords, starting on 16 bytes
        if format != DXGI_FORMAT_R32_TYPELESS || stride != 0 || !first.is_multiple_of(4) {
            return false;
        }
        4
    } else if stride > 0 {
        // structured: no format, a stride the host takes
        if format != DXGI_FORMAT_UNKNOWN || stride > 2048 || !stride.is_multiple_of(4) {
            return false;
        }
        u64::from(stride)
    } else {
        // typed: a format with a size that is not typeless
        let typeless = family(f) == f && family(f + 1) == f;
        match crate::resource::element(f) {
            Some((bytes, 1)) if f != 0 && !typeless && bytes > 0 => bytes,
            _ => return false,
        }
    };
    count > 0
        && first
            .checked_add(u64::from(count))
            .and_then(|end| end.checked_mul(element))
            .is_some_and(|end| end <= width)
}

fn buffer_srv(
    address: u64,
    len: u64,
    mut desc: D3D12_SHADER_RESOURCE_VIEW_DESC,
    at: D3D12_CPU_DESCRIPTOR_HANDLE,
) -> Option<Kept> {
    let shadow = crate::shadow::find(address, len)?;
    let offset = shadow.offset(address);
    // A structured view as a raw view of the same bytes (see `raw_window`).
    // SAFETY: a buffer view's union member.
    let structured = unsafe { desc.Anonymous.Buffer };
    let window = (desc.Format == DXGI_FORMAT_UNKNOWN)
        .then(|| raw_window(offset, structured.FirstElement, structured.NumElements, structured.StructureByteStride))
        .flatten();
    if let Some((first, count)) = window {
        desc.Format = DXGI_FORMAT_R32_TYPELESS;
        desc.Anonymous.Buffer = D3D12_BUFFER_SRV {
            FirstElement: first,
            NumElements: count,
            StructureByteStride: 0,
            Flags: D3D12_BUFFER_SRV_FLAG_RAW,
        };
    }
    let element = element_size(&desc);
    // SAFETY: a buffer view's union member.
    let buffer = unsafe { &mut desc.Anonymous.Buffer };
    if window.is_none() {
        if element == 0 || offset % element != 0 {
            once!(
                offset,
                element,
                "buffer view not on an element of its host copy"
            );
            return None;
        }
        buffer.FirstElement += offset / element;
    }
    let (first, count, stride) = (buffer.FirstElement, buffer.NumElements, buffer.StructureByteStride);
    let raw = buffer.Flags.0 & D3D12_BUFFER_SRV_FLAG_RAW.0 != 0;
    // SAFETY: a live buffer.
    let width = unsafe { shadow.buffer.GetDesc() }.Width;
    if !buffer_view_ok(desc.Format, first, count, stride, raw, width) {
        REFUSED_VIEWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        once!(
            format = desc.Format.0,
            first,
            count,
            stride,
            raw,
            width,
            "buffer view the host would refuse left null"
        );
        return None;
    }
    // The console has no component mapping; one the host does not know is the default's.
    if desc.Shader4ComponentMapping != D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING {
        desc.Shader4ComponentMapping = D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING;
    }
    let host = host()?;
    // SAFETY: a live buffer and a slot of our heap.
    unsafe {
        host.device
            .CreateShaderResourceView(&shadow.buffer, Some(&desc), at)
    };
    Some(Some(shadow.buffer.clone()))
}

/// An unordered access view of title buffer memory at `address` (`len`
/// bytes) on its host copy, as [`buffer_srv`]; false if the copy cannot be
/// written by shaders (upload memory).
/// shortcut: what shaders write stays in the host copy; title memory (and
/// so the CPU, and copies run on the CPU) does not see it.
fn buffer_uav(
    address: u64,
    len: u64,
    mut desc: D3D12_UNORDERED_ACCESS_VIEW_DESC,
    at: D3D12_CPU_DESCRIPTOR_HANDLE,
) -> Option<Kept> {
    let shadow = crate::shadow::find(address, len)?;
    // SAFETY: a live buffer.
    let flags = unsafe { shadow.buffer.GetDesc() }.Flags;
    if flags.0 & D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS.0 == 0 {
        once!("unordered access to a buffer whose host copy shaders cannot write");
        return None;
    }
    // SAFETY: a buffer view's union member.
    let buffer = unsafe { &mut desc.Anonymous.Buffer };
    let element = if buffer.StructureByteStride > 0 {
        u64::from(buffer.StructureByteStride)
    } else if buffer.Flags.0 & D3D12_BUFFER_UAV_FLAG_RAW.0 != 0 {
        4
    } else {
        crate::resource::element(desc.Format.0 as u32).map_or(0, |(bytes, _)| bytes)
    };
    let offset = shadow.offset(address);
    // A structured view as a raw view of the same bytes (see `raw_window`).
    let window = (desc.Format == DXGI_FORMAT_UNKNOWN)
        .then(|| raw_window(offset, buffer.FirstElement, buffer.NumElements, buffer.StructureByteStride))
        .flatten();
    if let Some((first, count)) = window {
        buffer.FirstElement = first;
        buffer.NumElements = count;
        buffer.StructureByteStride = 0;
        buffer.Flags = D3D12_BUFFER_UAV_FLAG_RAW;
    } else {
        if element == 0 || offset % element != 0 {
            once!(offset, element, "unordered buffer view not on an element of its host copy");
            return None;
        }
        buffer.FirstElement += offset / element;
    }
    buffer.CounterOffsetInBytes = 0;
    let (first, count, stride) = (buffer.FirstElement, buffer.NumElements, buffer.StructureByteStride);
    let raw = buffer.Flags.0 & D3D12_BUFFER_UAV_FLAG_RAW.0 != 0;
    if window.is_some() {
        // (Left out at first: every structured unordered access view was then
        // refused by `buffer_view_ok` and left null, runs 15 and 16.)
        desc.Format = DXGI_FORMAT_R32_TYPELESS;
    }
    // SAFETY: a live buffer.
    let width = unsafe { shadow.buffer.GetDesc() }.Width;
    if !buffer_view_ok(desc.Format, first, count, stride, raw, width) {
        REFUSED_VIEWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        once!(
            format = desc.Format.0,
            first,
            count,
            stride,
            raw,
            width,
            "unordered buffer view the host would refuse left null"
        );
        return None;
    }
    let host = host()?;
    // SAFETY: a live buffer and a slot of our heap.
    unsafe {
        host.device
            .CreateUnorderedAccessView(&shadow.buffer, None::<&ID3D12Resource>, Some(&desc), at)
    };
    Some(Some(shadow.buffer.clone()))
}

/// Whether the host can make a shader (or unordered access) view of
/// `texture` in `format` (none: the texture's own). The console allows any
/// view; the host refuses (and removes the device for) views without the
/// resource's permission, of typed depth, and in another format than a
/// typed resource's. A view of a typed texture in another format of its
/// family takes the texture's format, except an RGBA8 shader view that
/// asks for sRGB or linear on a host that allows the cast
/// ([`keeps_gamma_view`]).
/// shortcut: other such views (BGRA8 sRGB of UNORM, say) still read
/// without their conversion.
fn fits(texture: &ID3D12Resource, mut format: Option<&mut DXGI_FORMAT>, unordered: bool) -> bool {
    use crate::resource::{DEPTH_FORMATS, family};
    // SAFETY: a live resource.
    let d = unsafe { texture.GetDesc() };
    let allowed = if unordered {
        d.Flags.0 & D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS.0 != 0
    } else {
        d.Flags.0 & D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE.0 == 0
    };
    let own = d.Format.0 as u32;
    if !allowed || DEPTH_FORMATS.iter().any(|&(typed, _)| typed == own) {
        if unordered {
            crate::resource::DEPTH_UNORDERED[3].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        once!(
            unordered,
            format = own,
            flags = d.Flags.0,
            "view the host does not allow"
        );
        return false;
    }
    // Every family has more than one format, its typeless one first.
    let typeless = family(own) == own && family(own + 1) == own;
    if let Some(format) = format.as_deref_mut()
        && unordered
        && let Some(&(_, linear)) = crate::resource::SRGB
            .iter()
            .find(|&&(s, _)| s == format.0 as u32)
    {
        format.0 = linear as i32;
    }
    let view = format.as_ref().map_or(0, |f| f.0 as u32);
    if view == 0 {
        return !typeless;
    }
    // A view of a target whose format the host refuses, in the title's
    // format: in the stand-in the host copy has.
    if view != own && crate::resource::target_format(view) == own {
        if let Some(format) = format {
            *format = d.Format;
        }
        return true;
    }
    if family(view) != family(own) {
        once!(view, own, "view format of another family");
        return false;
    }
    if let Some(format) = format
        && !typeless
        && !keeps_gamma_view(own, view, unordered)
    {
        *format = d.Format;
    }
    true
}

/// Whether a shader view in format `view` of a typed texture in format
/// `own` keeps its own format: an RGBA8 view that picks sRGB or linear,
/// on a host that allows views of typed textures in other formats of their
/// family. Replacing the format there dropped the conversion, so an sRGB
/// view of a UNORM texture read its texels as linear (#578).
/// Unordered access views have no sRGB on the host and stay as they were.
fn keeps_gamma_view(own: u32, view: u32, unordered: bool) -> bool {
    const RGBA8_UNORM: u32 = DXGI_FORMAT_R8G8B8A8_UNORM.0 as u32;
    const RGBA8_SRGB: u32 = DXGI_FORMAT_R8G8B8A8_UNORM_SRGB.0 as u32;
    // VICTORY_VIEW_SRGB=0: the view takes the texture's format, as before.
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let on = *ON.get_or_init(|| std::env::var_os("VICTORY_VIEW_SRGB").is_none_or(|v| v != "0"));
    let gamma_pair = matches!((own, view), (RGBA8_UNORM, RGBA8_SRGB) | (RGBA8_SRGB, RGBA8_UNORM));
    on && !unordered && gamma_pair && host().is_some_and(|gpu| gpu.typed_format_casting())
}

/// The title's unordered access view of a depth texture, as a view of the
/// texture's colour twin `twin` ([`crate::resource::Twin`]): the same mip
/// and slices, in the colour format of the twin's family for a depth or
/// typeless format (R16_UNORM for 16-bit depth, R32_FLOAT for 32-bit; the
/// bits are the same). Without a description: mip 0 of every slice.
/// The twin of a stencil plane (R8) stands for the views of that plane
/// only, which become R8_UINT views of its one plane; `None` for a view of
/// the depth plane, which the twin does not stand for. The depth plane has
/// a twin of its own, viewed as a depth texture's without stencil: a view
/// in the format of the depth plane of a depth-stencil format (21, 46)
/// takes the twin's colour format too.
fn twin_uav(
    twin: &ID3D12Resource,
    desc: Option<&D3D12_UNORDERED_ACCESS_VIEW_DESC>,
) -> Option<D3D12_UNORDERED_ACCESS_VIEW_DESC> {
    // SAFETY: a live resource.
    let d = unsafe { twin.GetDesc() };
    let mut desc = desc.cloned().unwrap_or_else(|| {
        let mut all = D3D12_UNORDERED_ACCESS_VIEW_DESC {
            ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        if d.DepthOrArraySize > 1 {
            all.ViewDimension = D3D12_UAV_DIMENSION_TEXTURE2DARRAY;
            all.Anonymous.Texture2DArray = D3D12_TEX2D_ARRAY_UAV {
                ArraySize: u32::from(d.DepthOrArraySize),
                ..Default::default()
            };
        }
        all
    });
    // A stencil plane's twin (R8 typeless, 60): the title's view is of the
    // stencil plane if it names plane 1 or a stencil view format (22, 47).
    if d.Format.0 == 60 {
        let mut plane = 0;
        match desc.ViewDimension {
            D3D12_UAV_DIMENSION_TEXTURE2D => {
                // SAFETY: the member the dimension names.
                let mut view = unsafe { desc.Anonymous.Texture2D };
                plane = std::mem::take(&mut view.PlaneSlice);
                desc.Anonymous.Texture2D = view;
            }
            D3D12_UAV_DIMENSION_TEXTURE2DARRAY => {
                // SAFETY: the member the dimension names.
                let mut view = unsafe { desc.Anonymous.Texture2DArray };
                plane = std::mem::take(&mut view.PlaneSlice);
                desc.Anonymous.Texture2DArray = view;
            }
            _ => {}
        }
        if plane != 1 && !matches!(desc.Format.0, 22 | 47) {
            return None;
        }
        desc.Format.0 = 62;
        return Some(desc);
    }
    // None, typeless and depth formats (R16: 0, 53, 55; R32: 0, 39, 40),
    // and those of a depth-stencil format's depth plane (19 to 21, 44 to
    // 46), for the twin of such a plane.
    desc.Format.0 = match (desc.Format.0, d.Format.0) {
        (0 | 53 | 55 | 19..=21 | 44..=46, 53) => 56,
        (0 | 39 | 40 | 19..=21 | 44..=46, 39) => 41,
        (other, _) => other,
    };
    Some(desc)
}

/// The size of a constant buffer view the host takes: 256-byte steps, and
/// at most 4,096 vectors (65,536 bytes, `D3D12_REQ_CONSTANT_BUFFER_ELEMENT_COUNT`).
/// The title names constant views of more (one of 1,306,368 bytes, in the
/// world); `CreateConstantBufferView` with such a size is an invalid call,
/// and NVIDIA removes the device for it (`DXGI_ERROR_INVALID_CALL`), in
/// the frame the view is made. A shader reads at most 4,096 vectors through
/// a constant view, so nothing it can read is lost.
fn constant_view_size(size: u32) -> u32 {
    const MOST: u32 = 4096 * 16;
    if size > MOST {
        once!(size, "a constant buffer view over 64 KiB, made 64 KiB (the host takes no more)");
        return MOST;
    }
    size.max(1).next_multiple_of(256)
}

/// The host copy a constant buffer of `size` bytes at title `address` is
/// read from in place, as a raw view (`make`), and the bytes of it the
/// view takes: `size`, or less where the copy ends first. `None` if no
/// copy holds it or its bytes do not start on 16 in the copy (the host's
/// raw view alignment): such a constant is copied for the draw instead
/// (`draw::State::tables`). The same ask names the copy for the slot and
/// for the refresh before a draw (`buffer_copy`).
/// (Port of upstream 683ad7a9, Marvis; the fallback is this tree's own.)
pub(crate) fn constant_copy(address: u64, size: u32) -> Option<(std::sync::Arc<crate::shadow::Shadow>, u64)> {
    let shadow = crate::shadow::find(address, u64::from(size).max(1))
        .or_else(|| crate::shadow::find(address, 1))?;
    shadow.offset_in(address)?;
    let len = u64::from(size).min(shadow.left(address)) & !3;
    (len > 0 && shadow.offset(address).is_multiple_of(16)).then_some((shadow, len))
}

/// Makes a raw shader resource view of `len` bytes of `buffer` from byte
/// `offset` (on 16) at `at`; `None` if the host would refuse it.
fn raw_srv(buffer: &ID3D12Resource, offset: u64, len: u64, at: D3D12_CPU_DESCRIPTOR_HANDLE) -> Option<()> {
    let host = host()?;
    let (first, count) = (offset / 4, u32::try_from(len / 4).ok()?);
    // SAFETY: a live buffer.
    let width = unsafe { buffer.GetDesc() }.Width;
    if !offset.is_multiple_of(16) || !buffer_view_ok(DXGI_FORMAT_R32_TYPELESS, first, count, 0, true, width) {
        once!(offset, len, width, "a constant buffer's raw view the host would refuse left null");
        return None;
    }
    let desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
        Format: DXGI_FORMAT_R32_TYPELESS,
        ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Buffer: D3D12_BUFFER_SRV {
                FirstElement: first,
                NumElements: count,
                StructureByteStride: 0,
                Flags: D3D12_BUFFER_SRV_FLAG_RAW,
            },
        },
    };
    // SAFETY: a live buffer and a slot of our heap; the range lies in it.
    unsafe { host.device.CreateShaderResourceView(buffer, Some(&desc), at) };
    Some(())
}

/// Makes the host descriptor for `view` at `at`: the host resource it
/// views, or `None` if it cannot.
fn make(view: &View, at: D3D12_CPU_DESCRIPTOR_HANDLE) -> Option<Kept> {
    let host = host()?;
    let device = &host.device;
    match view {
        &View::Constant(address, size) => {
            // Constant buffers in the title's slots, through the heap itself
            // or a table, are read as raw buffers by translated shaders
            // (`victory_shader::emit`, "Constant buffers in the heap"): the
            // console needs 16-byte alignment, a host constant buffer view
            // 256, which most of the title's do not have on the host.
            // (Port of upstream 683ad7a9, Marvis, "table constants read in
            // place".)
            let (shadow, len) = constant_copy(address, size)?;
            raw_srv(&shadow.buffer, shadow.offset(address), len, at)?;
            Some(Some(shadow.buffer.clone()))
        }
        View::Shader(resource, desc) => {
            let resource = *resource as crate::object::This;
            if let Some(texture) = crate::resource::host_resource(resource) {
                // The title's views of textures with a stencil plane, as it
                // describes them (format, dimension, component mapping, a
                // pad, then first level, levels, plane): each distinct one
                // once. They say how its shaders read the stencil plane
                // (the lighting takes each pixel's material class and its
                // "inside a room" bit from it).
                #[cfg(feature = "diagnostics")]
                {
                    // SAFETY: a live resource.
                    let own = unsafe { texture.GetDesc() }.Format.0;
                    if matches!(own, 19..=22 | 44..=47) {
                        static SEEN: Mutex<Vec<(i32, Option<[u32; 10]>)>> = Mutex::new(Vec::new());
                        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
                        if seen.len() < 64 && !seen.contains(&(own, *desc)) {
                            seen.push((own, *desc));
                            tracing::info!(
                                ?resource,
                                texture_format = own,
                                view = ?desc,
                                "shader view of a depth-stencil texture"
                            );
                        }
                    }
                }
                let mut desc = desc.as_ref().map(srv_desc);
                if let Some(d) = desc.as_mut() {
                    shift_view_levels(crate::resource::dropped(resource), d);
                }
                if !fits(texture, desc.as_mut().map(|d| &mut d.Format), false) {
                    return None;
                }
                // SAFETY: a live texture and a slot of our heap.
                unsafe {
                    device.CreateShaderResourceView(
                        texture,
                        desc.as_ref().map(std::ptr::from_ref),
                        at,
                    )
                };
                Some(Some(texture.clone()))
            } else if let (Some((data, len)), Some(desc)) =
                (crate::resource::buffer_bytes(resource), desc)
            {
                buffer_srv(data as u64, len, srv_desc(desc), at)
            } else {
                None
            }
        }
        View::Unordered(resource, _counter, desc) => {
            let resource = *resource as crate::object::This;
            let Some(texture) = crate::resource::host_resource(resource) else {
                return match (crate::resource::buffer_bytes(resource), desc) {
                    (Some((data, len)), Some(desc)) => {
                        buffer_uav(data as u64, len, uav_desc(desc), at)
                    }
                    _ => None,
                };
            };
            // The first such views of depth textures are logged as the
            // title describes them (format, dimension, then mip, slices and
            // plane): they say which planes shaders write, and as what.
            if crate::resource::depth_and_unordered(resource) {
                static LOGGED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if LOGGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 512 {
                    tracing::info!(
                        ?resource,
                        view = ?desc,
                        "unordered access view of a depth texture"
                    );
                }
            }
            let mut desc = desc.as_ref().map(uav_desc);
            // A depth texture's unordered access is its colour twin's: the
            // host gives a depth texture none.
            let twin = crate::resource::twin_resource(resource)
                .and_then(|twin| Some((twin, twin_uav(twin, desc.as_ref())?)))
                // A view of the depth plane of a texture with a stencil
                // plane: of that plane's own twin.
                .or_else(|| {
                    let twin = crate::resource::twin_plane_resource(resource)?;
                    Some((twin, twin_uav(twin, desc.as_ref())?))
                });
            let texture = match twin {
                Some((twin, view)) => {
                    desc = Some(view);
                    twin
                }
                None => texture,
            };
            if !fits(texture, desc.as_mut().map(|d| &mut d.Format), true) {
                return None;
            }
            // SAFETY: a live texture and a slot of our heap.
            unsafe {
                device.CreateUnorderedAccessView(
                    texture,
                    None::<&ID3D12Resource>,
                    desc.as_ref().map(std::ptr::from_ref),
                    at,
                )
            };
            Some(Some(texture.clone()))
        }
        View::Sampler(words) => {
            // SAFETY: 13 words in the host's layout.
            let desc = unsafe {
                std::ptr::from_ref(words)
                    .cast::<D3D12_SAMPLER_DESC>()
                    .read_unaligned()
            };
            // SAFETY: a slot of our sampler heap.
            unsafe { device.CreateSampler(&desc, at) };
            Some(None)
        }
        View::ConsoleSampler(words) => {
            let desc = crate::sampler::console(words);
            // SAFETY: a slot of our sampler heap.
            unsafe { device.CreateSampler(&desc, at) };
            Some(None)
        }
        View::Placed(unordered, resource, desc) => {
            if let Some(view) = placed_texture(view) {
                return make(&view, at);
            }
            const BUFFER: u32 = 1;
            if resource[0] != BUFFER {
                // shortcut: unordered access to a texture the title placed
                // itself is not mirrored; give it a resource as shader
                // views of such textures get when a dispatch writes one.
                once!(dimension = resource[0], "placed unordered texture view not mirrored");
                return None;
            }
            // The resource description's width, after the alignment.
            let len = u64::from(resource[4]) | u64::from(resource[5]) << 32;
            let address = u64::from(desc[10]) | u64::from(desc[11]) << 32;
            let words: &[u32; 10] = desc[..10].try_into().unwrap_or(&[0; 10]);
            if *unordered {
                buffer_uav(address, len, uav_desc(words), at)
            } else {
                buffer_srv(address, len, srv_desc(words), at)
            }
        }
    }
}

/// Writes of a slot with another view of the placed texture it showed,
/// since the start: the slot kept showing the old view.
pub(crate) static KEPT_VIEWS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Whether a slot written with another view of the placed texture it
/// shows keeps showing the old one until the new is made;
/// `VICTORY_KEEP_VIEWS=0` makes it null at once, as before.
fn keep_views() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_KEEP_VIEWS").is_none_or(|v| v != "0"))
}

/// Whether both are shader views of one texture the title placed in
/// memory: the same address and the same resource description, whatever
/// part of it each shows.
fn same_placed_texture(a: &View, b: &View) -> bool {
    match (a, b) {
        (View::Placed(false, resource, desc), View::Placed(false, other, other_desc)) => {
            resource[0] != 1
                && crate::resource::placed_desc_key(resource)
                    == crate::resource::placed_desc_key(other)
                && desc[10..12] == other_desc[10..12]
        }
        _ => false,
    }
}

/// A shader view of a texture the title placed in memory itself, as a view
/// of the resource made for that memory (on first use).
pub(crate) fn placed_texture(view: &View) -> Option<View> {
    const BUFFER: u32 = 1;
    let View::Placed(false, resource, desc) = view else {
        return None;
    };
    if resource[0] == BUFFER {
        return None;
    }
    let address = u64::from(desc[10]) | u64::from(desc[11]) << 32;
    let texture = crate::resource::texture_at(address, resource)?;
    Some(View::Shader(texture as usize, desc[..10].try_into().ok()))
}

/// A null view of the kind `view` is, for a slot whose view the host
/// cannot make: unordered access (to a buffer or a 2D texture) or a shader
/// view. Writes to it are dropped, reads give zeros.
fn null_for(view: &View, at: D3D12_CPU_DESCRIPTOR_HANDLE) {
    let Some(host) = host() else { return };
    let is_buffer = |r: &usize| crate::resource::buffer_bytes(*r as crate::object::This).is_some();
    // Unordered views: whether a buffer, and the title's view dimension.
    let (buffer, dimension) = match view {
        View::Unordered(r, _, desc) => (
            is_buffer(r) || desc.is_some_and(|d| d[1] == 1),
            desc.map(|d| d[1]),
        ),
        View::Placed(true, resource, desc) => (resource[0] == 1, Some(desc[1])),
        // Shader views of buffers read nothing through a null buffer view:
        // a null 2D view where the shader expects a buffer is a mismatch
        // the hardware does not survive in general (GPU-based validation
        // reports it).
        View::Shader(r, desc) if is_buffer(r) || desc.is_some_and(|d| d[1] == 1) => {
            return null_buffer_view(at);
        }
        View::Placed(false, resource, _) if resource[0] == 1 => return null_buffer_view(at),
        // Textures: a null view of the title's dimension (1D, arrays, 3D,
        // cubes), not always 2D.
        View::Shader(_, Some(desc)) => return null_like(desc, at),
        View::Placed(false, _, desc) => {
            return null_like(desc[..10].try_into().unwrap_or(&[0; 10]), at);
        }
        // A constant buffer's slot holds a raw view (`make`): a null one.
        View::Constant(..) => return null_buffer_view(at),
        _ => return null_view(at),
    };
    let desc = if buffer {
        D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: DXGI_FORMAT_R32_UINT,
            ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
            ..Default::default()
        }
    } else {
        null_uav_desc(dimension)
    };
    // SAFETY: a slot of our heap; a null resource is allowed with a desc.
    unsafe {
        host.device.CreateUnorderedAccessView(
            None::<&ID3D12Resource>,
            None::<&ID3D12Resource>,
            Some(&desc),
            at,
        )
    };
}

/// A null texture UAV of the title's view dimension (1D, 1D or 2D arrays,
/// 3D), 2D if it names none: a 2D null view where the shader writes a 3D
/// texture is a mismatch GPU-based validation reports (the world's wind
/// field, CS_CopyWindFieldBeforeUpdateWithSmooth, 2026-11-24).
fn null_uav_desc(dimension: Option<u32>) -> D3D12_UNORDERED_ACCESS_VIEW_DESC {
    let mut desc = D3D12_UNORDERED_ACCESS_VIEW_DESC {
        Format: DXGI_FORMAT_R32_UINT,
        ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2D,
        ..Default::default()
    };
    match dimension.map(|d| D3D12_UAV_DIMENSION(d as i32)) {
        Some(D3D12_UAV_DIMENSION_TEXTURE1D) => {
            desc.ViewDimension = D3D12_UAV_DIMENSION_TEXTURE1D;
        }
        Some(D3D12_UAV_DIMENSION_TEXTURE1DARRAY) => {
            desc.ViewDimension = D3D12_UAV_DIMENSION_TEXTURE1DARRAY;
            desc.Anonymous.Texture1DArray = D3D12_TEX1D_ARRAY_UAV {
                ArraySize: 1,
                ..Default::default()
            };
        }
        Some(D3D12_UAV_DIMENSION_TEXTURE2DARRAY) => {
            desc.ViewDimension = D3D12_UAV_DIMENSION_TEXTURE2DARRAY;
            desc.Anonymous.Texture2DArray = D3D12_TEX2D_ARRAY_UAV {
                ArraySize: 1,
                ..Default::default()
            };
        }
        Some(D3D12_UAV_DIMENSION_TEXTURE3D) => {
            desc.ViewDimension = D3D12_UAV_DIMENSION_TEXTURE3D;
            desc.Anonymous.Texture3D = D3D12_TEX3D_UAV {
                WSize: u32::MAX,
                ..Default::default()
            };
        }
        _ => {}
    }
    desc
}

/// A buffer view that reads zeros (raw, as translated shaders mostly read
/// buffers).
fn null_buffer_view(at: D3D12_CPU_DESCRIPTOR_HANDLE) {
    let Some(host) = host() else { return };
    let desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
        Format: DXGI_FORMAT_R32_TYPELESS,
        ViewDimension: D3D12_SRV_DIMENSION_BUFFER,
        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Buffer: D3D12_BUFFER_SRV {
                NumElements: 1,
                Flags: D3D12_BUFFER_SRV_FLAG_RAW,
                ..Default::default()
            },
        },
    };
    // SAFETY: a slot of our heap; a null resource is allowed with a desc.
    unsafe {
        host.device
            .CreateShaderResourceView(None::<&ID3D12Resource>, Some(&desc), at)
    };
}

/// A texture view that reads zeros with the dimension (and mip and array
/// counts) of the title's description `words`; 2D if it names none.
fn null_like(words: &[u32; 10], at: D3D12_CPU_DESCRIPTOR_HANDLE) {
    const TEXTURES: std::ops::RangeInclusive<u32> = 2..=10;
    if !TEXTURES.contains(&words[1]) {
        return null_view(at);
    }
    let Some(host) = host() else { return };
    #[cfg(feature = "diagnostics")]
    if let Some(debug) = debug_null()
        && debug.view(&debug.known, words[1], at)
    {
        return;
    }
    // One mip, one slice or cube, plane 0: fields a null view still checks.
    let mut w = [0u32; 10];
    w[1] = words[1];
    match words[1] {
        6 => {}
        7 => w[5] = 1,
        3 | 5 | 10 => (w[5], w[7]) = (1, 1),
        _ => w[5] = 1,
    }
    let mut desc = srv_desc(&w);
    desc.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    desc.Shader4ComponentMapping = D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING;
    // SAFETY: a slot of our heap; a null resource is allowed with a desc.
    unsafe {
        host.device
            .CreateShaderResourceView(None::<&ID3D12Resource>, Some(&desc), at)
    };
}

/// A view that reads zeros, for slots whose view the host cannot make.
/// shortcut: slots with no view at all (kind unknown) get this null 2D
/// view; null buffer views there did not stop the world's low-address
/// page faults (2026-11-24, run w10), so they are not the cause.
fn null_view(at: D3D12_CPU_DESCRIPTOR_HANDLE) {
    let Some(host) = host() else { return };
    #[cfg(feature = "diagnostics")]
    if let Some(debug) = debug_null()
        && debug.view(&debug.unknown, D3D12_SRV_DIMENSION_TEXTURE2D.0 as u32, at)
    {
        return;
    }
    let desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D12_TEX2D_SRV {
                MipLevels: 1,
                ..Default::default()
            },
        },
    };
    // SAFETY: a slot of our heap; a null resource is allowed with a desc.
    unsafe {
        host.device
            .CreateShaderResourceView(None::<&ID3D12Resource>, Some(&desc), at)
    };
}

/// `VICTORY_DEBUG_NULL_VIEWS=1` (diagnostics builds): texture slots that
/// would read zeros read a checkerboard instead, so a surface whose texture
/// is missing shows as a pattern where it would be black, and black that is
/// left is the title's own (unlit, or drawn black):
/// - magenta: the title made a texture view there, and the host has no
///   copy of the texture (not uploaded yet, unmapped, refused, of a format
///   or kind it cannot make);
/// - cyan: no view the host knows at all (never written, or past the host
///   heap's cap).
///
/// 2D and 2D-array slots only; 1D, 3D, cube and multisampled ones still
/// read zeros (a stand-in of another dimension is a mismatch the hardware
/// need not survive).
#[cfg(feature = "diagnostics")]
struct DebugNull {
    known: ID3D12Resource,
    unknown: ID3D12Resource,
}

// SAFETY: D3D12 resources are free-threaded; these are only read.
#[cfg(feature = "diagnostics")]
unsafe impl Send for DebugNull {}
// SAFETY: as above.
#[cfg(feature = "diagnostics")]
unsafe impl Sync for DebugNull {}

#[cfg(feature = "diagnostics")]
impl DebugNull {
    /// An 8x8 checkerboard of `colour` and black, RGBA8, in memory the CPU
    /// writes once and shaders read from then on.
    fn checker(device: &ID3D12Device, colour: [u8; 4]) -> windows::core::Result<ID3D12Resource> {
        const SIDE: u32 = 8;
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_CUSTOM,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_WRITE_BACK,
            MemoryPoolPreference: D3D12_MEMORY_POOL_L0,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: u64::from(SIDE),
            Height: SIDE,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            ..Default::default()
        };
        let texels: Vec<u8> = (0..SIDE * SIDE)
            .flat_map(|i| {
                if (i % SIDE + i / SIDE).is_multiple_of(2) {
                    colour
                } else {
                    [0, 0, 0, 255]
                }
            })
            .collect();
        let mut texture: Option<ID3D12Resource> = None;
        // SAFETY: creation of a CPU-writable texture, then one write of
        // `texels` (SIDE rows of SIDE * 4 bytes) into its only subresource.
        unsafe {
            device.CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE
                    | D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
                None,
                &mut texture,
            )?;
            let texture = texture.ok_or_else(windows::core::Error::empty)?;
            texture.Map(0, None, None)?;
            let written = texture.WriteToSubresource(
                0,
                None,
                texels.as_ptr().cast(),
                SIDE * 4,
                SIDE * SIDE * 4,
            );
            texture.Unmap(0, None);
            written?;
            Ok(texture)
        }
    }

    /// Makes `at` a view of `texture` if `dimension` (the title's SRV
    /// dimension) is 2D or a 2D array; whether it did.
    fn view(&self, texture: &ID3D12Resource, dimension: u32, at: D3D12_CPU_DESCRIPTOR_HANDLE) -> bool {
        let Some(host) = host() else { return false };
        let mut desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
            ..Default::default()
        };
        match D3D12_SRV_DIMENSION(dimension as i32) {
            D3D12_SRV_DIMENSION_TEXTURE2D => {
                desc.ViewDimension = D3D12_SRV_DIMENSION_TEXTURE2D;
                desc.Anonymous.Texture2D = D3D12_TEX2D_SRV {
                    MipLevels: 1,
                    ..Default::default()
                };
            }
            D3D12_SRV_DIMENSION_TEXTURE2DARRAY => {
                desc.ViewDimension = D3D12_SRV_DIMENSION_TEXTURE2DARRAY;
                desc.Anonymous.Texture2DArray = D3D12_TEX2D_ARRAY_SRV {
                    MipLevels: 1,
                    ArraySize: 1,
                    ..Default::default()
                };
            }
            _ => return false,
        }
        // SAFETY: a slot of our heap and a live texture the view fits.
        unsafe { host.device.CreateShaderResourceView(texture, Some(&desc), at) };
        true
    }
}

/// The stand-ins, if `VICTORY_DEBUG_NULL_VIEWS=1` (made on first use).
#[cfg(feature = "diagnostics")]
fn debug_null() -> Option<&'static DebugNull> {
    static DEBUG: std::sync::LazyLock<Option<DebugNull>> = std::sync::LazyLock::new(|| {
        if std::env::var_os("VICTORY_DEBUG_NULL_VIEWS").is_none_or(|v| v != "1") {
            return None;
        }
        let device = &host()?.device;
        match (
            DebugNull::checker(device, [255, 0, 255, 255]),
            DebugNull::checker(device, [0, 255, 255, 255]),
        ) {
            (Ok(known), Ok(unknown)) => {
                tracing::info!(
                    "VICTORY_DEBUG_NULL_VIEWS: texture slots without a host texture read a magenta checkerboard, \
                     slots without any view a cyan one"
                );
                Some(DebugNull { known, unknown })
            }
            (Err(e), _) | (_, Err(e)) => {
                tracing::warn!(error = %e, "VICTORY_DEBUG_NULL_VIEWS: no stand-in textures");
                None
            }
        }
    });
    DEBUG.as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_retired_ranges_join_in_both_directions_but_keep_gaps() {
        let ranges = vec![(10, 2), (12, 3), (9, 1), (2, 2), (5, 2), (4, 1)];
        assert_eq!(coalesced(ranges).collect::<Vec<_>>(), [(9, 6), (2, 2), (4, 3)]);
    }

    #[test]
    fn best_fit_preserves_a_long_run_for_a_later_table() {
        let mut runs = FreeRuns::new(64);
        assert_eq!(runs.take(64, false), Some(0));
        runs.release(0, 30);
        runs.release(40, 48);
        assert_eq!(runs.take(8, false), Some(40));
        assert_eq!(runs.take(30, false), Some(0));
        assert_eq!(runs.take(1, true), None);
    }

    #[test]
    fn submitted_ranges_are_retired_once_in_fence_order() {
        let mut ring = Ring::new(16, false);
        let a = ring.take(8, false).unwrap();
        let b = ring.take(8, false).unwrap();
        ring.retiring.push_back((7, vec![(a as u32, 8)]));
        ring.retiring.push_back((9, vec![(b as u32, 8)]));
        assert_eq!(ring.completed_ranges(6), None);
        assert_eq!(ring.completed_ranges(u64::MAX), None);
        assert_eq!(ring.take(1, false), None);
        for (start, n) in ring.completed_ranges(8).unwrap() {
            ring.release(start as usize, (start + n) as usize).unwrap();
        }
        assert_eq!(ring.take(8, false), Some(a));
        // A late landing of fence 7 cannot free the new owner's slots.
        assert_eq!(ring.completed_ranges(8), None);
        assert_eq!(ring.take(1, false), None);
        assert_eq!(ring.completed_ranges(9), Some(vec![(b as u32, 8)]));
        assert_eq!(ring.completed_ranges(9), None);
    }

    #[test]
    fn constant_views_are_at_most_64_kib() {
        assert_eq!(constant_view_size(0), 256);
        assert_eq!(constant_view_size(16), 256);
        assert_eq!(constant_view_size(256), 256);
        assert_eq!(constant_view_size(65_536), 65_536);
        assert_eq!(constant_view_size(1_306_368), 65_536);
        assert_eq!(constant_view_size(u32::MAX), 65_536);
    }

    /// Ranges past the limit are joined, never dropped: touching ones first,
    /// then across growing gaps, and every range stays covered.
    #[test]
    fn noted_ranges_are_joined_not_dropped() {
        let mut ranges = vec![(300, 400), (0, 100), (100, 200), (1 << 20, (1 << 20) + 10)];
        coalesce(&mut ranges, 3);
        assert_eq!(ranges, vec![(0, 200), (300, 400), (1 << 20, (1 << 20) + 10)]);
        let original: Vec<(u64, u64)> = (0..1000u64).map(|i| (i * 100_000, i * 100_000 + 4096)).collect();
        let mut ranges = original.clone();
        coalesce(&mut ranges, 100);
        assert!(ranges.len() <= 100);
        for &(start, end) in &original {
            assert!(ranges.iter().any(|&(s, e)| s <= start && end <= e), "{start}..{end} left out");
        }
    }

    /// The unsigned twin of a 16-bit float or signed format has its layout,
    /// and only buffer views in such a format are shown as another view.
    #[test]
    fn only_16_bit_float_and_signed_buffer_views_get_an_unsigned_twin() {
        for (format, unsigned) in [(10, 12), (14, 12), (34, 36), (38, 36), (54, 57), (59, 57)] {
            assert_eq!(half_format(format), Some(unsigned));
            assert_eq!(
                crate::resource::element(format),
                crate::resource::element(unsigned),
                "format {format}"
            );
        }
        // The unsigned formats themselves, normalised 16-bit formats, and
        // wider and narrower ones.
        for other in [0, 12, 36, 57, 11, 13, 35, 37, 56, 58, 42, 17, 30, 61] {
            assert_eq!(half_format(other), None, "format {other}");
        }
        const BUFFER: u32 = 1;
        let words = |format: u32, dimension: u32| [format, dimension, 5768, 0, 0, 0, 64, 0, 0, 0];
        // R16G16B16A16_FLOAT: a buffer of halves a program moves as they are.
        assert_eq!(
            half_view(&View::Unordered(7, 0, Some(words(10, BUFFER)))),
            Some(View::Unordered(7, 0, Some(words(12, BUFFER))))
        );
        assert_eq!(
            half_view(&View::Shader(7, Some(words(54, BUFFER)))),
            Some(View::Shader(7, Some(words(57, BUFFER))))
        );
        let mut placed = [0u32; 16];
        placed[..10].copy_from_slice(&words(38, BUFFER));
        placed[10] = 0x1234_0000;
        let mut twin = placed;
        twin[0] = 36;
        let memory = [BUFFER, 0, 0, 0, 512, 0, 1, 0x1_0001, 0, 1, 0, 1, 0, 0];
        assert_eq!(
            half_view(&View::Placed(true, memory, placed)),
            Some(View::Placed(true, memory, twin))
        );
        // The title's kind (R16G16B16A16_UINT and R16_UINT buffers), a
        // texture view (dimension 4: 2D), a view without a description, a
        // constant buffer: the title's own.
        assert_eq!(
            half_view(&View::Unordered(7, 0, Some(words(12, BUFFER)))),
            None
        );
        assert_eq!(half_view(&View::Shader(7, Some(words(57, BUFFER)))), None);
        assert_eq!(half_view(&View::Unordered(7, 0, Some(words(10, 4)))), None);
        assert_eq!(half_view(&View::Shader(7, None)), None);
        assert_eq!(half_view(&View::Constant(0x1000, 256)), None);
    }

    /// The buffer views through which the integers a shader moves halves
    /// as are not what the console gives are named; the title's are not.
    #[test]
    fn views_whose_values_the_console_would_convert_are_named() {
        const BUFFER: u32 = 1;
        let words = |format: u32, dimension: u32| [format, dimension, 5768, 0, 0, 0, 64, 0, 0, 0];
        for exact in [10, 12, 14, 34, 36, 38, 54, 57, 59, 25, 30, 50, 62] {
            let view = View::Unordered(7, 0, Some(words(exact, BUFFER)));
            assert_eq!(half_inexact(&view), None, "format {exact}");
        }
        // R32_FLOAT, R32_UINT, R16G16B16A16_UNORM, R8G8B8A8_UNORM, R8_SINT,
        // and a structured or raw view (no format).
        for inexact in [41, 42, 11, 28, 64, 0] {
            let view = View::Shader(7, Some(words(inexact, BUFFER)));
            assert_eq!(half_inexact(&view), Some(inexact));
        }
        let mut placed = [0u32; 16];
        placed[..10].copy_from_slice(&words(41, BUFFER));
        let memory = [BUFFER, 0, 0, 0, 512, 0, 1, 0x1_0001, 0, 1, 0, 1, 0, 0];
        assert_eq!(half_inexact(&View::Placed(true, memory, placed)), Some(41));
        // Not a buffer view.
        assert_eq!(half_inexact(&View::Shader(7, Some(words(41, 4)))), None);
        assert_eq!(half_inexact(&View::Constant(0x1000, 256)), None);
    }

    /// The two ways a translated `buffer_load_format_d16_xyzw` and
    /// `buffer_store_format_d16_xyzw` have moved a typed buffer's 16-bit
    /// channels, on the host device, over a buffer that holds every 16-bit
    /// pattern. As unsigned integers through an unsigned view (the title's
    /// own R16G16B16A16_UINT, or the twin `half_view` gives a float one)
    /// every pattern comes back as it was. By way of floats, as before
    /// (load as floats, each to its half's bits; each half widened and
    /// stored as a float): through the title's integer view that is
    /// outside what D3D12 defines, and what the device gives is printed,
    /// not checked (1 pattern of 65,536 on the cards tried: the network
    /// that moves the title's faces computed zeros).
    #[test]
    fn halves_cross_an_unsigned_buffer_view_unchanged() {
        const MOVES: &str = r#"
#define RS "DescriptorTable(UAV(u0, numDescriptors=2))"
#ifdef BY_FLOATS
RWBuffer<float4> from : register(u0);
RWBuffer<float4> to : register(u1);
#else
RWBuffer<uint4> from : register(u0);
RWBuffer<uint4> to : register(u1);
#endif
[RootSignature(RS)]
[numthreads(64, 1, 1)]
void BY_FLOATS_OR_NOT(uint3 id : SV_DispatchThreadID) {
#ifdef BY_FLOATS
    uint4 h = f32tof16(from[id.x]);
#else
    uint4 h = from[id.x] & 0xffff;
#endif
    uint low = h.x | h.y << 16;
    uint high = h.z | h.w << 16;
#ifdef BY_FLOATS
    to[id.x] = float4(f16tof32(low), f16tof32(low >> 16), f16tof32(high), f16tof32(high >> 16));
#else
    to[id.x] = uint4(low & 0xffff, low >> 16, high & 0xffff, high >> 16);
#endif
}
"#;
        const ELEMENTS: usize = 1 << 14;
        const BYTES: u64 = ELEMENTS as u64 * 8;
        const TITLES: u32 = 12; // R16G16B16A16_UINT
        let Some(host) = host() else {
            return; // no host GPU
        };
        let heap: ID3D12DescriptorHeap = {
            let desc = D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                NumDescriptors: 2,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                NodeMask: 0,
            };
            // SAFETY: plain heap creation.
            unsafe { host.device.CreateDescriptorHeap(&desc) }.expect("a view heap")
        };
        // How many of the 65,536 patterns come back as they were, moved by
        // the program `by_floats` or not, through views in `format`.
        let moved = |by_floats: bool, format: u32| -> usize {
            let source = if by_floats {
                MOVES
                    .replace("BY_FLOATS_OR_NOT", "main")
                    .replace("#ifdef BY_FLOATS", "#if 1")
            } else {
                MOVES
                    .replace("BY_FLOATS_OR_NOT", "main")
                    .replace("#ifdef BY_FLOATS", "#if 0")
            };
            let code = crate::host::compile(
                &source,
                windows::core::s!("main"),
                windows::core::s!("cs_5_1"),
            )
            .expect("the test's shader");
            let (from, filled) = host.shared_buffer(BYTES).expect("a buffer");
            let (to, result) = host.shared_buffer(BYTES).expect("a buffer");
            // SAFETY: the shader's own root signature and plain creation
            // calls; both mappings hold `BYTES` bytes; the views are of
            // live buffers, in slots of the heap made above; an open list.
            unsafe {
                let root: ID3D12RootSignature = host
                    .device
                    .CreateRootSignature(0, &code)
                    .expect("a root signature");
                let pso: ID3D12PipelineState = host
                    .device
                    .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                        pRootSignature: std::mem::transmute_copy(&root),
                        CS: D3D12_SHADER_BYTECODE {
                            pShaderBytecode: code.as_ptr().cast(),
                            BytecodeLength: code.len(),
                        },
                        ..Default::default()
                    })
                    .expect("a pipeline");
                let filled = std::slice::from_raw_parts_mut(filled.cast::<u16>(), ELEMENTS * 4);
                for (pattern, half) in filled.iter_mut().enumerate() {
                    *half = pattern as u16;
                }
                std::ptr::write_bytes(result, 0xee, BYTES as usize);
                let view = D3D12_UNORDERED_ACCESS_VIEW_DESC {
                    Format: DXGI_FORMAT(format as i32),
                    ViewDimension: D3D12_UAV_DIMENSION_BUFFER,
                    Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 {
                        Buffer: D3D12_BUFFER_UAV {
                            NumElements: ELEMENTS as u32,
                            ..Default::default()
                        },
                    },
                };
                let start = heap.GetCPUDescriptorHandleForHeapStart();
                let step = host
                    .device
                    .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV)
                    as usize;
                host.device.CreateUnorderedAccessView(
                    &from,
                    None::<&ID3D12Resource>,
                    Some(&view),
                    start,
                );
                let second = D3D12_CPU_DESCRIPTOR_HANDLE {
                    ptr: start.ptr + step,
                };
                host.device.CreateUnorderedAccessView(
                    &to,
                    None::<&ID3D12Resource>,
                    Some(&view),
                    second,
                );
                let list = host.new_list().expect("a host list");
                list.list.SetDescriptorHeaps(&[Some(heap.clone())]);
                list.list.SetComputeRootSignature(&root);
                list.list.SetPipelineState(&pso);
                list.list
                    .SetComputeRootDescriptorTable(0, heap.GetGPUDescriptorHandleForHeapStart());
                list.list.Dispatch(ELEMENTS as u32 / 64, 1, 1);
                list.close();
                host.execute(&[&list]);
                assert!(
                    host.device.GetDeviceRemovedReason().is_ok(),
                    "the device, after views in format {format}"
                );
                let result = std::slice::from_raw_parts(result.cast::<u16>(), ELEMENTS * 4);
                (0..=u16::MAX)
                    .filter(|&p| result[usize::from(p)] == p)
                    .count()
            }
        };
        // A float view of halves is shown as the title's kind of view.
        assert_eq!(half_format(10), Some(TITLES));
        let unsigned = moved(false, TITLES);
        let by_floats = moved(true, TITLES);
        eprintln!(
            "halves through an R16G16B16A16_UINT buffer view: {unsigned} of 65536 patterns came back unchanged moved as unsigned integers, {by_floats} moved by way of floats"
        );
        assert_eq!(
            unsigned, 65_536,
            "patterns unchanged, moved as unsigned integers"
        );
    }

    /// A null texture UAV keeps the title's dimension (3D for the world's
    /// wind field), and is 2D when the title names none.
    #[test]
    fn null_uavs_keep_the_titles_dimension() {
        let d = null_uav_desc(Some(8));
        assert_eq!(d.ViewDimension, D3D12_UAV_DIMENSION_TEXTURE3D);
        // SAFETY: the member the dimension names.
        assert_eq!(unsafe { d.Anonymous.Texture3D.WSize }, u32::MAX);
        let d = null_uav_desc(Some(5));
        assert_eq!(d.ViewDimension, D3D12_UAV_DIMENSION_TEXTURE2DARRAY);
        // SAFETY: the member the dimension names.
        assert_eq!(unsafe { d.Anonymous.Texture2DArray.ArraySize }, 1);
        let dimension = |d| null_uav_desc(d).ViewDimension;
        assert_eq!(dimension(Some(2)), D3D12_UAV_DIMENSION_TEXTURE1D);
        assert_eq!(dimension(Some(4)), D3D12_UAV_DIMENSION_TEXTURE2D);
        assert_eq!(dimension(None), D3D12_UAV_DIMENSION_TEXTURE2D);
        // Multisampled UAVs need a cap; they stay 2D.
        assert_eq!(dimension(Some(6)), D3D12_UAV_DIMENSION_TEXTURE2D);
    }

    /// Views of one placed texture are told from views of others by the
    /// address and the resource description, not by what they show.
    #[test]
    fn views_of_one_placed_texture_are_told_by_address_and_description() {
        let texture = [3, 0, 0, 0, 512, 0, 512, 0x8_0001, 98, 1, 0, 0x105, 0, 0];
        let view = |resource: [u32; 14], first_mip: u32, address: u32| {
            let mut desc = [0u32; 16];
            (desc[0], desc[1], desc[4], desc[5], desc[10], desc[11]) = (98, 4, first_mip, 8 - first_mip, address, 0xb);
            View::Placed(false, resource, desc)
        };
        let low = view(texture, 3, 0x1000_0000);
        // more mips of the same texture
        assert!(same_placed_texture(&low, &view(texture, 0, 0x1000_0000)));
        // different stack-filler words [1] and [13] still name the same texture
        let mut with_filler = texture;
        with_filler[1] = 0xDEAD_BEEF;
        with_filler[13] = 0xCAFE_BABE;
        assert!(same_placed_texture(&low, &view(with_filler, 0, 0x1000_0000)));
        // Spans::bytes deduplicates descriptions differing only in filler words
        let mut spans = Spans::default();
        assert_eq!(spans.bytes(&texture), spans.bytes(&with_filler));
        assert_eq!(spans.sizes.len(), 1);
        // another address, another description, a buffer, an unordered view
        assert!(!same_placed_texture(&low, &view(texture, 3, 0x2000_0000)));
        let mut wider = texture;
        wider[4] = 1024;
        assert!(!same_placed_texture(&low, &view(wider, 3, 0x1000_0000)));
        let mut buffer = texture;
        buffer[0] = 1;
        assert!(!same_placed_texture(&view(buffer, 3, 0x1000_0000), &view(buffer, 3, 0x1000_0000)));
        let View::Placed(_, resource, desc) = low.clone() else { unreachable!() };
        assert!(!same_placed_texture(&low, &View::Placed(true, resource, desc)));
        assert!(!same_placed_texture(&low, &View::Shader(0, None)));
    }

    /// A null 3D UAV made on the real device leaves it alive.
    #[test]
    fn null_3d_uav_is_accepted_by_the_device() {
        let Some(host) = host() else {
            return; // no host GPU
        };
        let Some(heap) = HostHeap::new(false, 4) else {
            return;
        };
        let view = View::Unordered(0, 0, Some([28, 8, 0, 0, 0, 0, 0, 0, 0, 0]));
        null_for(&view, heap.cpu(0));
        // SAFETY: a plain device query.
        assert!(unsafe { host.device.GetDeviceRemovedReason() }.is_ok());
    }

    /// Dynamic slots stay taken until given back, and runs do not wrap.
    #[test]
    fn a_structured_view_made_raw_passes_the_host_check() {
        // 100 elements of 20 bytes from element 8, in a copy that starts 32
        // bytes before the view's memory: bytes 192..2192.
        let (first, count) = raw_window(32, 8, 100, 20).unwrap();
        assert_eq!((first, count), (48, 500));
        assert!(buffer_view_ok(DXGI_FORMAT_R32_TYPELESS, first, count, 0, true, 4096));
        // the format a raw view must name; the structured one's is refused
        assert!(!buffer_view_ok(DXGI_FORMAT_UNKNOWN, first, count, 0, true, 4096));
        // past the buffer, not on 16 bytes, no elements
        assert!(!buffer_view_ok(DXGI_FORMAT_R32_TYPELESS, first, count, 0, true, 2000));
        assert!(raw_window(4, 0, 100, 20).is_none());
        assert!(!buffer_view_ok(DXGI_FORMAT_R32_TYPELESS, 0, 0, 0, true, 4096));
        // a structured view the host takes as it is
        assert!(buffer_view_ok(DXGI_FORMAT_UNKNOWN, 1, 10, 12, false, 4096));
    }

    /// The queue of textures picked by index serves the slots looked at
    /// before first, then the newest write and the oldest in turns; a
    /// slot whose memory is not there is looked at again after
    /// 1, 2, 4 frames up to the limit; and what the pass reports about a
    /// slot written again meanwhile is dropped.
    #[test]
    fn the_texture_queue_serves_the_newest_writes_first() {
        let mut q = TextureQueue::default();
        // Slots 10..18 written in frames 1..9, slot 5 last.
        for i in 0..8u32 {
            q.written(10 + i, u64::from(i) + 1, 0, false);
        }
        q.written(5, 9, 0, false);
        let order: Vec<u32> = q.due(9, 100).0.into_iter().map(|d| d.0).collect();
        // newest, oldest, newest, oldest, and so on
        assert_eq!(order, [5, 10, 17, 11, 16, 12, 15, 13, 14]);
        assert_eq!(q.due(9, 2).0.len(), 2);
        assert_eq!(q.due(9, 0).1, [9, 9, 0, 0, 8]);
        // Slot 17's memory is not mapped: again next frame, then in 2, 4, 4.
        let write = |q: &TextureQueue, index: u32| {
            q.due(u64::MAX, 100).0.into_iter().find(|d| d.0 == index).unwrap().1
        };
        let w17 = write(&q, 17);
        let mut frame = 9;
        for wait in [1, 2, 4, 4] {
            q.later(17, w17, frame, 4);
            assert!(!q.due(frame + wait - 1, 100).0.iter().any(|d| d.0 == 17));
            frame += wait;
            // Due again, and ahead of every slot never looked at.
            assert_eq!(q.due(frame, 100).0[0].0, 17);
        }
        assert_eq!(q.due(9, 0).1[2], 1);
        q.done(17, w17);
        assert!(!q.due(u64::MAX, 100).0.iter().any(|d| d.0 == 17));
        // Uploaded with part of its memory there: kept, with what was there.
        let w16 = write(&q, 16);
        q.partly(16, w16, 12, 4096);
        assert_eq!(q.mapped(16), 4096);
        assert_eq!(q.due(12, 100).0[0].0, 16);
        // The title writes slot 16 again while the pass works on it: what
        // the pass then says about the old write changes nothing.
        q.written(16, 13, 0, false);
        q.done(16, w16);
        q.later(16, w16, 13, 4);
        assert_eq!(q.mapped(16), 0);
        assert_eq!(q.due(13, 100).0[0].0, 16);
        // A draw that makes the host slot (`seen`) keeps what the queue knows,
        let w15 = write(&q, 15);
        q.partly(15, w15, 20, 8192);
        q.written(15, 14, 0, true);
        assert_eq!(q.mapped(15), 8192);
        // and queues a slot the queue did not hold.
        q.written(99, 14, 0, true);
        assert!(q.due(14, 100).0.iter().any(|d| d.0 == 99));
        // A write due later (a table's slot) is not handed out before.
        q.written(98, 14, 16, false);
        assert!(!q.due(15, 100).0.iter().any(|d| d.0 == 98));
        assert!(q.due(16, 100).0.iter().any(|d| d.0 == 98));
    }

    /// Before a bake draw, a slot waiting for its first look after a whole
    /// upload and a slot uploaded in part are due at once; a slot whose
    /// memory is not there yet, and one looked at twice, keep their turn.
    /// (After fork PR 1097 a8bcf21d by Azemed.)
    #[test]
    fn a_bake_pulls_rechecks_and_partial_uploads_forward() {
        let mut q = TextureQueue::default();
        for i in 1..=4 {
            q.written(i, 1, 0, false);
        }
        let write = |q: &TextureQueue, index: u32| q.due(u64::MAX, 100).0.into_iter().find(|d| d.0 == index).unwrap().1;
        let (w1, w2, w3, w4) = (write(&q, 1), write(&q, 2), write(&q, 3), write(&q, 4));
        q.recheck(1, w1, 40, (0x1000, 4096), None, 0xfeed, 1, 1);
        q.partly(2, w2, 40, 2048);
        q.later(3, w3, 20, 64);
        q.recheck(4, w4, 40, (0x2000, 4096), None, 0xbeef, 2, 1);
        assert!(q.due(10, 100).0.is_empty());
        assert_eq!(q.bake_prepare(10), 2);
        let mut due: Vec<u32> = q.due(10, 100).0.into_iter().map(|d| d.0).collect();
        due.sort_unstable();
        assert_eq!(due, [1, 2]);
        // Already due: nothing more to pull.
        assert_eq!(q.bake_prepare(10), 0);
    }

    /// While the card is full, more than 256 refused slots ahead of the
    /// others do not end every pass: a recheck, a remapped slot, one in the
    /// middle of the fresh ones and the last retry all get a turn, and with
    /// room again every refused slot is due at once. (After fork PR 676
    /// f130ba1a by AscilCH.)
    #[test]
    fn a_full_card_does_not_starve_the_slots_behind_the_refused_ones() {
        let mut q = TextureQueue::default();
        q.written(1, 1, 0, false);
        let w1 = q.due(1, 10).0[0].1;
        q.recheck(1, w1, 2, (0x1000, 4096), None, 0xdead_beef, 0, 1);
        for i in 100..400u32 {
            q.written(i, 2, 0, false);
            let w = q.slots[&i].write;
            q.later(i, w, 2, 16);
        }
        q.changed(50, 3, 3);
        for i in 1000..1400u32 {
            q.written(i, 3, 0, false);
        }
        let mut seen = [false; 4];
        for frame in 3..12u64 {
            let mut refused = 0;
            for (index, write) in q.due_for(frame, 4096, false).0 {
                if let Some(at) = [1, 50, 1200, 100].iter().position(|&s| s == index) {
                    seen[at] = true;
                    q.done(index, write);
                    continue;
                }
                q.full(index, write, frame, 16);
                refused += 1;
                if refused > 256 {
                    break;
                }
            }
        }
        assert_eq!(seen, [true; 4], "recheck, remapped, middle, last retry");
        assert_eq!(q.due_for(12, 4096, true).0.len(), 698);
    }

    #[test]
    fn a_limited_texture_pass_keeps_the_full_queue_priority_and_counts() {
        let mut q = TextureQueue::default();
        for (frame, slot) in [3, 12, 5, 8, 11, 2, 10, 6, 7, 4, 1, 9].into_iter().enumerate() {
            q.written(slot, frame as u64, 0, false);
        }
        q.later(5, 3, 11, 4);
        q.partly(11, 5, 12, 4096);
        q.later(6, 8, 11, 4);
        q.partly(10, 7, 13, 4096);
        q.written(4, 11, 14, false);

        // Three retries first, newest write first. The remaining seven
        // alternate between newest and oldest, with no slot repeated.
        let priority = [
            (6, 8),
            (11, 5),
            (5, 3),
            (9, 12),
            (3, 1),
            (1, 11),
            (12, 2),
            (7, 9),
            (8, 4),
            (2, 6),
        ];
        for max in 0..=priority.len() + 1 {
            let (ready, counts) = q.due(12, max);
            assert_eq!(ready, priority[..max.min(priority.len())], "max={max}");
            assert_eq!(counts, [12, 10, 2, 2, 12], "max={max}");
        }
    }

    /// Textures of which only the smallest levels are held, packed into
    /// one another's address range (the three of run 188: 0x2c000 bytes
    /// each at full size, their starts 0x2000 and 0x4000 apart): writing
    /// any of them, in any order and again, leaves the others.
    #[test]
    fn packed_neighbours_stay() {
        let mut spans = Spans::default();
        assert_eq!(spans.rule, Rule::Start);
        let at = 0xb_86bf_8000u64;
        let (a, b, c) = (at, at + 0x2000, at + 0x6000);
        assert!(spans.note(105_998, a, a + 0x2c000, true).is_empty());
        assert!(spans.note(106_004, b, b + 0x2c000, true).is_empty());
        assert!(spans.note(108_521, c, c + 0x2c000, true).is_empty());
        // The first written again (as a title does when levels arrive),
        // from another slot too: its span reaches over the other two.
        assert!(spans.note(7, a, a + 0x2c000, true).is_empty());
        assert!(spans.note(106_004, b, b + 0x2c000, true).is_empty());
        let mut all = spans.slots_in(0, u64::MAX);
        all.sort_unstable();
        assert_eq!(all, [7, 105_998, 106_004, 108_521]);
        // A draw makes a slot of the second texture: it is one of them.
        assert!(spans.note(9, b, b + 0x2c000, false).is_empty());
        assert_eq!(spans.by_slot.get(&9), Some(&b));
    }

    /// A texture written where another starts replaces it: at the same
    /// address with another length, or inside the block of its smallest
    /// levels. The packed neighbours of the one replaced stay.
    #[test]
    fn a_texture_written_at_the_start_of_another_replaces_it() {
        let mut spans = Spans::default();
        let at = 0x7_0000_0000u64;
        assert!(spans.note(1, at, at + 0x2c000, true).is_empty());
        assert!(spans.note(2, at + 0x2000, at + 0x2e000, true).is_empty());
        // Another length from the same start: slot 1 is stale, slot 2 not.
        assert_eq!(spans.note(3, at, at + 0xb000, true), [1]);
        // A start inside the first 4 KiB of slot 3's texture: stale too.
        assert_eq!(spans.note(4, at + 0x800, at + 0x1800, true), [3]);
        // And the other way: a texture that starts just before one.
        assert_eq!(spans.note(5, at + 0x1800, at + 0x4800, true), [2]);
        let mut left = spans.slots_in(0, u64::MAX);
        left.sort_unstable();
        assert_eq!(left, [4, 5]);
        // Exactly 4 KiB apart: both there.
        assert!(spans.note(6, at + 0x2800, at + 0x3800, true).is_empty());
        // A draw's view that clashes is left out, and nothing is stale.
        assert!(spans.note(8, at + 0x900, at + 0x2900, false).is_empty());
        assert_eq!(spans.by_slot.get(&8), None);
        assert_eq!(spans.by_slot.len(), 3);
    }

    /// A slot written with another texture leaves its old span; the same
    /// view written again changes nothing.
    #[test]
    fn a_slot_rewritten_moves_to_its_new_span() {
        let mut spans = Spans::default();
        assert!(spans.note(1, 0x10000, 0x3c000, true).is_empty());
        assert!(spans.note(2, 0x12000, 0x3e000, true).is_empty());
        assert!(spans.note(1, 0x10000, 0x3c000, true).is_empty());
        assert_eq!(spans.slots_in(0x10000, 0x10001), [1]);
        // Slot 1 now views a texture far away.
        assert!(spans.note(1, 0x90000, 0x91000, true).is_empty());
        assert_eq!(spans.slots_in(0x10000, 0x10001), [] as [u32; 0]);
        assert_eq!(spans.slots_in(0x12000, 0x12001), [2]);
        assert_eq!(spans.slots_in(0x90000, 0x90001), [1]);
        spans.forget(1);
        spans.forget(2);
        assert!(spans.by_start.is_empty() && spans.by_slot.is_empty());
    }

    /// An RGBA8 texture read through an sRGB or linear view gets that
    /// view's conversion, whichever of the two the texture itself is (#578).
    /// Without the cast on the host, the view keeps the texture's format, as
    /// before. Samples one texel of 0x80 on the GPU.
    #[test]
    fn rgba8_views_keep_their_srgb_conversion() {
        const UNORM: DXGI_FORMAT = DXGI_FORMAT_R8G8B8A8_UNORM;
        const SRGB: DXGI_FORMAT = DXGI_FORMAT_R8G8B8A8_UNORM_SRGB;
        // 0x80 read as linear, and as sRGB converted to linear.
        const LINEAR_VALUE: f32 = 128.0 / 255.0;
        const SRGB_VALUE: f32 = 0.215_860_5;
        let Some(gpu) = host() else {
            return; // no host GPU
        };
        let casting = gpu.typed_format_casting();
        for (texture_format, view_format) in [(UNORM, UNORM), (UNORM, SRGB), (SRGB, UNORM), (SRGB, SRGB)] {
            let read_as = if casting { view_format } else { texture_format };
            let expected = if read_as == SRGB { SRGB_VALUE } else { LINEAR_VALUE };
            let red = sample_one_texel(&gpu.device, texture_format, view_format).expect("the sample ran");
            assert!(
                (red - expected).abs() < 0.002,
                "texture {} viewed as {}: red {red}, expected {expected}",
                texture_format.0,
                view_format.0
            );
        }
    }

    /// Fills a 1x1 texture in `texture_format` with 0x80 in every channel,
    /// makes a shader view of it in `view_format` the way the title's views
    /// are made ([`fits`]), and returns the red channel a compute shader
    /// reads through that view.
    fn sample_one_texel(
        device: &ID3D12Device,
        texture_format: DXGI_FORMAT,
        view_format: DXGI_FORMAT,
    ) -> windows::core::Result<f32> {
        const SHADER: &str = "#define RS \"DescriptorTable(SRV(t0)), UAV(u0)\"\n\
            Texture2D<float4> texel : register(t0);\n\
            RWByteAddressBuffer result : register(u0);\n\
            [RootSignature(RS)]\n\
            [numthreads(1, 1, 1)]\n\
            void main() { result.Store(0, asuint(texel.Load(int3(0, 0, 0)).r)); }\n";
        // Rows of a texture copy are 256 bytes apart.
        const ROW_PITCH: u64 = 256;
        let mut rig = crate::inplace::Rig::new(device)?;
        let default_heap = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() };
        let texture_desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: 1,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: texture_format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            ..Default::default()
        };
        let result_desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: 4,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
            ..Default::default()
        };
        let mut texture: Option<ID3D12Resource> = None;
        let mut result: Option<ID3D12Resource> = None;
        // SAFETY: plain creation of resources of our own.
        unsafe {
            device.CreateCommittedResource(
                &default_heap,
                D3D12_HEAP_FLAG_NONE,
                &texture_desc,
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut texture,
            )?;
            device.CreateCommittedResource(
                &default_heap,
                D3D12_HEAP_FLAG_NONE,
                &result_desc,
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut result,
            )?;
        }
        let texture = texture.ok_or_else(windows::core::Error::empty)?;
        let result = result.ok_or_else(windows::core::Error::empty)?;

        // The texel's bytes, uploaded through a buffer.
        let upload = rig.committed(D3D12_HEAP_TYPE_UPLOAD, ROW_PITCH, D3D12_RESOURCE_STATE_GENERIC_READ)?;
        let mut mapped = std::ptr::null_mut();
        // SAFETY: the mapping holds the buffer's bytes until unmapped.
        unsafe {
            upload.Map(0, None, Some(&mut mapped))?;
            mapped.cast::<u8>().write_bytes(0x80, 4);
            upload.Unmap(0, None);
        }

        // The view, made as the title's are: `fits` picks its format.
        let mut view_desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: view_format,
            ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
            Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2D: D3D12_TEX2D_SRV { MipLevels: 1, ..Default::default() },
            },
        };
        assert!(fits(&texture, Some(&mut view_desc.Format), false), "the host allows the view");
        // SAFETY: plain creation; the view names a live texture.
        let view_heap: ID3D12DescriptorHeap = unsafe {
            let heap: ID3D12DescriptorHeap = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                NumDescriptors: 1,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                NodeMask: 0,
            })?;
            device.CreateShaderResourceView(&texture, Some(&view_desc), heap.GetCPUDescriptorHandleForHeapStart());
            heap
        };

        let code = crate::host::compile(SHADER, windows::core::s!("main"), windows::core::s!("cs_5_1"))?;
        // SAFETY: the compiled bytes carry the root signature too.
        let (root, pipeline): (ID3D12RootSignature, ID3D12PipelineState) = unsafe {
            let root: ID3D12RootSignature = device.CreateRootSignature(0, &code)?;
            let pipeline = device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                CS: D3D12_SHADER_BYTECODE { pShaderBytecode: code.as_ptr().cast(), BytecodeLength: code.len() },
                ..Default::default()
            })?;
            (root, pipeline)
        };

        // Upload the texel, then let the shader read it.
        // SAFETY: an open list of ours; every resource named is alive.
        rig.run(|list| unsafe {
            let destination = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::transmute_copy(&texture),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
            };
            let source = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::transmute_copy(&upload),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                        Offset: 0,
                        Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                            Format: texture_format,
                            Width: 1,
                            Height: 1,
                            Depth: 1,
                            RowPitch: ROW_PITCH as u32,
                        },
                    },
                },
            };
            list.CopyTextureRegion(&destination, 0, 0, 0, &source, None);
            list.ResourceBarrier(&[crate::host::transition(
                &texture,
                D3D12_RESOURCE_STATE_COPY_DEST,
                D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            )]);
            list.SetDescriptorHeaps(&[Some(view_heap.clone())]);
            list.SetComputeRootSignature(&root);
            list.SetPipelineState(&pipeline);
            list.SetComputeRootDescriptorTable(0, view_heap.GetGPUDescriptorHandleForHeapStart());
            list.SetComputeRootUnorderedAccessView(1, result.GetGPUVirtualAddress());
            list.Dispatch(1, 1, 1);
        })?;
        let bytes = rig.fetch(&result, 0, 4)?;
        Ok(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Memory the runtime maps, unmaps or writes names the textures that
    /// are there: among packed neighbours the one whose held levels the
    /// range lies in (it ends where the next one starts), and the ones
    /// that start inside the range.
    #[test]
    fn memory_changed_under_packed_neighbours_names_the_ones_there() {
        let mut spans = Spans::default();
        let at = 0x7_0000_0000u64;
        assert!(spans.note(1, at, at + 0x2c000, true).is_empty());
        assert!(spans.note(2, at + 0x2000, at + 0x2e000, true).is_empty());
        assert!(spans.note(3, at + 0x6000, at + 0x32000, true).is_empty());
        // Inside the first one's held levels.
        assert_eq!(spans.slots_in(at + 0x1000, at + 0x2000), [1]);
        // Inside the second's: not the first, which ends where it starts.
        assert_eq!(spans.slots_in(at + 0x3000, at + 0x4000), [2]);
        // Past the last start: the last one.
        assert_eq!(spans.slots_in(at + 0x20000, at + 0x21000), [3]);
        // A range over all of them.
        assert_eq!(spans.slots_in(at, at + 0x8000), [1, 2, 3]);
        // Past the end of all: none.
        assert_eq!(spans.slots_in(at + 0x32000, at + 0x33000), [] as [u32; 0]);
    }

    /// `VICTORY_STALE_TEXTURES=0`: nothing is ever stale, and a view that
    /// overlaps others is left out of the spans, as before.
    #[test]
    fn never_stale_leaves_overlapping_views_out() {
        let mut spans = Spans { rule: Rule::Never, ..Default::default() };
        assert!(spans.note(1, 0x1000, 0x3000, true).is_empty());
        assert!(spans.note(2, 0x2000, 0x4000, true).is_empty());
        assert!(spans.note(3, 0x1000, 0x2000, true).is_empty());
        assert_eq!(spans.slots_in(0, u64::MAX), [1]);
    }

    /// The rule until run 188 (`VICTORY_STALE_TEXTURES=overlap`): a view
    /// written over memory that older views describe otherwise makes
    /// those stale; views of the same span are one texture's.
    #[test]
    fn a_newer_view_of_other_memory_makes_the_older_stale() {
        let mut spans = Spans { rule: Rule::Overlap, ..Default::default() };
        // Three textures side by side, the first in two slots.
        assert!(spans.note(1, 0x1000, 0x3000, true).is_empty());
        assert!(spans.note(2, 0x1000, 0x3000, true).is_empty());
        assert!(spans.note(3, 0x3000, 0x4000, true).is_empty());
        assert!(spans.note(4, 0x8000, 0x9000, true).is_empty());
        // The same slot written with the same span again: nothing changes.
        assert!(spans.note(2, 0x1000, 0x3000, true).is_empty());
        assert_eq!(spans.slots_in(0x2000, 0x3800), [1, 2, 3]);
        assert_eq!(spans.slots_in(0x4000, 0x8000), [] as [u32; 0]);
        // A draw makes a slot whose view overlaps them: left out, nothing stale.
        assert!(spans.note(9, 0x2000, 0x3800, false).is_empty());
        assert_eq!(spans.slots_in(0x2000, 0x3800), [1, 2, 3]);
        // The title writes a texture over the end of the first and all of
        // the second: their three slots are stale.
        let mut stale = spans.note(5, 0x2000, 0x4000, true);
        stale.sort_unstable();
        assert_eq!(stale, [1, 2, 3]);
        assert_eq!(spans.slots_in(0, u64::MAX), [5, 4]);
        // Another start in the same memory: stale in turn. Another length
        // from the same start too.
        assert_eq!(spans.note(6, 0x2800, 0x3000, true), [5]);
        assert_eq!(spans.note(7, 0x2800, 0x2c00, true), [6]);
        // A slot that moves to another texture leaves its span behind.
        assert!(spans.note(7, 0x8000, 0x9000, true).is_empty());
        assert_eq!(spans.slots_in(0x2800, 0x2c00), [] as [u32; 0]);
        assert_eq!(spans.slots_in(0x8000, 0x8001), [4, 7]);
        spans.forget(4);
        spans.forget(7);
        assert!(spans.by_start.is_empty() && spans.by_slot.is_empty());
    }

    /// Memory the runtime maps under a slot's texture makes the slot due
    /// again from the next frame; a stale slot leaves the queue.
    #[test]
    fn remapped_memory_makes_its_texture_slots_due() {
        let Some(heap) = HostHeap::new(false, 8) else {
            return; // no host GPU
        };
        let slots = |at: u64| -> Vec<u32> { heap.texture_slots_due(at, 8, true).0.into_iter().map(|d| d.0).collect() };
        // 64x64 BC7, one mip, at an address no test and no title uses.
        let at = 0x7fee_0000_0000u64;
        let view = |address: u64, width: u32| {
            let mut desc = [0u32; 16];
            (desc[10], desc[11]) = (address as u32, (address >> 32) as u32);
            View::Placed(false, [3, 0, 0, 0, width, 0, 64, 0x1_0001, 98, 1, 0, 0x105, 0, 0], desc)
        };
        let len = crate::resource::placed_bytes(&[3, 0, 0, 0, 64, 0, 64, 0x1_0001, 98, 1, 0, 0x105, 0, 0]).unwrap();
        assert!(heap.note_buffer(1, Some(&view(at, 64)), false).is_empty());
        let (queued, write) = heap.texture_slots_due(u64::MAX, 8, true).0[0];
        assert_eq!(queued, 1);
        heap.texture_slot_done(1, write, None);
        assert_eq!(slots(u64::MAX), []);
        // Memory elsewhere: nothing. Under the texture: due from the next frame.
        remapped((at + 2 * len) as usize, 0x1000);
        remapped(at as usize, 0x1000);
        assert_eq!(heap.texture_slots_remapped(50, false), 1);
        assert_eq!(slots(50), []);
        assert_eq!(slots(51), [1]);
        // Right before a bake draw: due in the same frame (fork PR 1097).
        let (_, write) = heap.texture_slots_due(51, 8, true).0[0];
        heap.texture_slot_done(1, write, None);
        remapped(at as usize, 0x1000);
        assert_eq!(heap.texture_slots_remapped(55, true), 1);
        assert_eq!(slots(55), [1]);
        let (_, write) = heap.texture_slots_due(55, 8, true).0[0];
        heap.texture_slot_done(1, write, None);
        // A wider texture written at the same address: slot 1 is stale.
        assert_eq!(heap.note_buffer(2, Some(&view(at, 128)), false), [1]);
        assert_eq!(slots(u64::MAX), [2]);
    }

    /// A slot a draw read through a table leaves the queue; the title's
    /// next write of it waits for its draw, and is due after the grace
    /// frames if none reads it.
    #[test]
    fn slots_read_through_tables_leave_the_texture_queue() {
        let Some(heap) = HostHeap::new(false, 8) else {
            return; // no host GPU
        };
        let frame = 100;
        let slots = |at: u64| -> Vec<u32> { heap.texture_slots_due(at, 8, true).0.into_iter().map(|d| d.0).collect() };
        let texture = View::Placed(false, [3, 0, 0, 0, 4, 0, 4, 0x1_0001, 28, 1, 0, 0x105, 0, 0], [0; 16]);
        assert!(is_texture_view(&texture));
        heap.queue_texture(2, true, false, frame);
        heap.queue_texture(3, true, false, frame);
        assert_eq!(slots(frame), [3, 2]);
        heap.tabled_in(3, true, frame);
        assert_eq!(slots(frame), [2]);
        // Made in place by the draw that read it: still out.
        heap.queue_texture(3, true, true, frame);
        assert_eq!(slots(frame), [2]);
        // Written again in the next frame: due once the grace frames have
        // passed without a draw that reads it.
        heap.queue_texture(3, true, false, frame + 1);
        assert_eq!(slots(frame + TABLED_GRACE), [2]);
        assert_eq!(slots(frame + 1 + TABLED_GRACE), [3, 2]);
        // The draw comes after all: out again.
        heap.tabled_in(3, true, frame + 1);
        assert_eq!(slots(u64::MAX), [2]);
        // A slot of a table no draw read stays queued; its next write waits.
        heap.tabled_in(2, false, frame + 1);
        assert_eq!(slots(frame + 1), [2]);
        heap.queue_texture(2, true, false, frame + 2);
        assert_eq!(slots(frame + 2), []);
        // Long after the last table, a write is due at once.
        heap.queue_texture(2, true, false, frame + 3 + TABLED_FRAMES);
        assert_eq!(slots(frame + 3 + TABLED_FRAMES), [2]);
    }

    #[test]
    fn dynamic_search_can_find_a_run_across_the_cursor() {
        let Some(mut heap) = HostHeap::new(false, 4) else {
            eprintln!("no host descriptor heap; skipped");
            return;
        };
        // The single search, whatever VICTORY_DYNAMIC_SPLIT says: the first
        // slot taken is the bottom one.
        heap.split = false;
        let first = heap.dynamic(1).unwrap();
        let left = heap.dynamic(2).unwrap();
        let right = heap.dynamic(2).unwrap();
        let rest = heap.dynamic(heap.dynamic - 5).unwrap();
        assert_eq!((first, left, right, rest), (4, 5, 7, 9));

        // Reallocating the left half puts the cursor back in the middle.
        heap.release(left, 2);
        assert_eq!(heap.dynamic(2), Some(left));
        heap.release(left, 2);
        heap.release(right, 2);
        assert_eq!(heap.dynamic(4), Some(left));
        assert_eq!(heap.dynamic(1), None);

        // A free slot at each physical end does not form a two-slot range.
        heap.release(first, 1);
        let last = first + heap.dynamic - 1;
        heap.release(last, 1);
        assert_eq!(heap.dynamic(2), None);
    }

    #[test]
    fn dynamic_slots_are_held_until_released() {
        let Some(heap) = HostHeap::new(false, 4) else {
            return; // no host GPU
        };
        let a = heap.dynamic(heap.dynamic - 100).unwrap();
        let b = heap.dynamic(60).unwrap();
        assert_eq!((a, b), (4, 4 + heap.dynamic - 100));
        // 40 left at the end: 50 do not fit until something is given back.
        assert_eq!(heap.dynamic(50), None);
        heap.release(a, 50);
        assert_eq!(heap.dynamic(50), Some(4));
        assert_eq!(heap.dynamic(40), Some(4 + heap.dynamic - 40));
    }

    /// Single slots taken from the top leave the bottom in long runs once
    /// the tables of eight between them are given back; taken among the
    /// tables they cut the free slots in runs of eight.
    #[test]
    fn dynamic_single_slots_split_from_tables_keep_runs_long() {
        let tables = |split: bool| {
            let mut ring = Ring::new(36, false);
            let mut eights = Vec::new();
            for _ in 0..4 {
                assert!(ring.take(1, split).is_some());
                eights.push(ring.take(8, split).unwrap());
            }
            assert_eq!((0..36).filter(|&at| ring.is_taken(at)).count(), 36);
            for first in eights {
                assert_eq!(ring.release(first, first + 8).map(|r| r.0), Some(8));
            }
            ring
        };
        // Split: the single slots at 35 down to 32, the tables below them.
        let mut ring = tables(true);
        assert_eq!(ring.take(16, true), Some(0));
        assert_eq!(ring.take(16, true), Some(16));
        assert_eq!(ring.take(1, true), None);
        // All given back, by either search: one run again.
        assert_eq!(ring.release(0, 36), Some((36, 36)));
        assert_eq!(ring.take(36, true), Some(0));
        // One search: four runs of eight between the single slots.
        let mut ring = tables(false);
        assert_eq!(ring.take(16, false), None);
        assert_eq!(ring.take(8, false), Some(1));
    }

    #[test]
    fn packed_reuse_keeps_released_tables_contiguous() {
        for pack in [false, true] {
            let mut ring = Ring::new(128, pack);
            for i in 0..8 {
                assert_eq!(ring.take(1, true), Some(127 - i));
                assert_eq!(ring.take(8, true), Some(8 * i));
            }
            ring.release(0, 8).unwrap();
            for at in 120..128 {
                ring.release(at, at + 1).unwrap();
            }
            for _ in 0..8 {
                ring.take(1, true).unwrap();
            }
            assert_eq!(ring.take(8, true), Some(if pack { 0 } else { 64 }));
            assert_eq!(ring.take(72, true), None);
            // Two adjacent live tables retire in pieces, against the unused tail.
            for at in (48..64).step_by(4) {
                assert_eq!(ring.release(at, at + 4).unwrap().0, 4);
            }
            assert_eq!((0..128).filter(|&at| !ring.is_taken(at)).count(), 72);
            assert_eq!(ring.largest_free_run(), if pack { 72 } else { 40 });
            assert_eq!(ring.take(72, true), pack.then_some(48));
        }
    }

    #[test]
    fn packed_search_matches_free_space_after_partial_releases() {
        let mut seed = 42u32;
        let mut random = |limit: usize| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as usize % limit
        };
        for (split, best_fit) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut ring = Ring::new(259, true);
            if best_fit {
                ring.free_runs = Some(FreeRuns::new(ring.len));
            }
            let mut reference = Reference { next: 0, top: 258, taken: vec![false; 259] };
            for _ in 0..4000 {
                if random(3) == 0 {
                    let at = random(259);
                    let end = (at + random(90)).min(259);
                    assert_eq!(ring.release(at, end), reference.release(at, end));
                } else {
                    let n = if random(3) == 0 { 1 } else { 1 + random(100) };
                    let got = ring.take(n, split);
                    assert_eq!(got.is_some(), n <= reference.longest());
                    if let Some(at) = got {
                        assert!(reference.taken[at..at + n].iter().all(|&used| !used));
                        reference.taken[at..at + n].fill(true);
                    }
                }
                assert_eq!(ring.largest_free_run(), reference.longest());
            }
        }
    }

    /// The downward search for single slots goes on from the top of the
    /// ring once it reaches the bottom, and never hands out a slot twice.
    #[test]
    fn dynamic_single_slots_wrap_downward() {
        let mut ring = Ring::new(4, false);
        assert_eq!(ring.take(1, true), Some(3));
        assert_eq!(ring.take(1, true), Some(2));
        assert_eq!(ring.release(3, 4), Some((1, 1)));
        assert_eq!(ring.take(1, true), Some(1));
        assert_eq!(ring.take(1, true), Some(0));
        assert_eq!(ring.take(1, true), Some(3));
        assert_eq!(ring.take(1, true), None);
        // Giving back a free slot gives back nothing.
        assert_eq!(ring.release(0, 1), Some((1, 1)));
        assert_eq!(ring.release(0, 1), Some((0, 1)));
    }

    /// The search over a flag per slot that [`Ring`] replaced, kept as the
    /// reference its word search must agree with.
    struct Reference {
        next: usize,
        top: usize,
        taken: Vec<bool>,
    }

    impl Reference {
        fn take(&mut self, n: usize, split: bool) -> Option<usize> {
            let len = self.taken.len();
            if n == 0 || n > len {
                return None;
            }
            let down = split && n == 1;
            let first = if down {
                let top = self.top.min(len - 1);
                (0..=top).rev().chain((top + 1..len).rev()).find(|&at| !self.taken[at])?
            } else {
                let cursor = self.next;
                let second_end = cursor.saturating_add(n.saturating_sub(1)).min(len);
                let mut run = 0;
                let mut last = usize::MAX;
                let mut found = None;
                for at in (cursor..len).chain(0..second_end) {
                    if at != last.wrapping_add(1) {
                        run = 0;
                    }
                    last = at;
                    run = if self.taken[at] { 0 } else { run + 1 };
                    if run == n {
                        found = Some(at + 1 - n);
                        break;
                    }
                }
                found?
            };
            self.taken[first..first + n].fill(true);
            if down {
                self.top = first.checked_sub(1).unwrap_or(len - 1);
            } else {
                self.next = (first + n) % len;
            }
            Some(first)
        }

        fn release(&mut self, at: usize, end: usize) -> Option<(u32, usize)> {
            let len = self.taken.len();
            let slots = self.taken.get_mut(at..end)?;
            let given = slots.iter().filter(|taken| **taken).count() as u32;
            slots.fill(false);
            if at == end {
                return Some((given, 0));
            }
            let mut start = at;
            while start > 0 && !self.taken[start - 1] {
                start -= 1;
            }
            let mut stop = end;
            while stop < len && !self.taken[stop] {
                stop += 1;
            }
            Some((given, stop - start))
        }

        /// The longest run of free slots.
        fn longest(&self) -> usize {
            let (mut run, mut longest) = (0, 0);
            for taken in &self.taken {
                run = if *taken { 0 } else { run + 1 };
                longest = longest.max(run);
            }
            longest
        }
    }

    /// The word search finds what the search over a flag per slot found,
    /// from the same cursors, through thousands of random tables and give
    /// backs (runs across words, across the cursors and up to the end of
    /// rings whose length is and is not a multiple of 64), and `too_long`
    /// kept as `HostHeap` keeps it never refuses a table that has room.
    #[test]
    fn dynamic_word_search_matches_the_slot_search() {
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        // xorshift64: a number below `below`.
        let mut random = move |below: usize| -> usize {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % below as u64) as usize
        };
        for len in [1, 5, 63, 64, 65, 130, 200, 256] {
            for split in [false, true] {
                let mut ring = Ring::new(len, false);
                let mut reference = Reference {
                    next: 0,
                    top: len - 1,
                    taken: vec![false; len],
                };
                let mut held: Vec<(usize, usize)> = Vec::new();
                let mut too_long = usize::MAX;
                for step in 0..4000 {
                    match random(10) {
                        0..=5 => {
                            let n = match random(4) {
                                0 => 1,
                                1 => 1 + random(8),
                                2 => 1 + random(70),
                                _ => 1 + random(len + 1),
                            };
                            let want = reference.take(n, split);
                            let got = if n < too_long { ring.take(n, split) } else { None };
                            assert_eq!(got, want, "len {len} split {split} step {step}: {n} slots");
                            match got {
                                Some(first) => held.push((first, first + n)),
                                None => too_long = too_long.min(n),
                            }
                        }
                        6..=8 if !held.is_empty() => {
                            let (at, end) = held.swap_remove(random(held.len()));
                            let got = ring.release(at, end);
                            assert_eq!(got, reference.release(at, end), "len {len} step {step}");
                            if got.is_some_and(|(_, run)| run >= too_long) {
                                too_long = usize::MAX;
                            }
                        }
                        _ => {
                            // Any range, taken or not, or past the end.
                            let at = random(len + 1);
                            let end = at + random(len + 2 - at);
                            let got = ring.release(at, end);
                            assert_eq!(got, reference.release(at, end), "len {len} step {step}: {at}..{end}");
                            if got.is_some_and(|(_, run)| run >= too_long) {
                                too_long = usize::MAX;
                            }
                        }
                    }
                    assert_eq!((ring.next, ring.top), (reference.next, reference.top));
                    assert!((0..len).all(|at| ring.is_taken(at) == reference.taken[at]));
                    assert!(reference.longest() < too_long, "len {len} step {step}");
                }
            }
        }
    }
}
