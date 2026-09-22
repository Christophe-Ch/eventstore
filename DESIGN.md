# Design

The authoritative spec for the on-disk format. Changing the format means changing this
file first. Anything written to disk by a previous version must remain readable, or the
change is a breaking one and needs saying so here.

## Conventions

All integers are **little-endian**. It matches the hardware this runs on, and it means
integers can eventually be read straight out of a memory mapping with no conversion.

## Record layout

The log is a sequence of records, back to back, starting at offset 0. There is no file
header.

```
offset  size  field
------  ----  -----------------------------------------
     0     4  length   u32, byte length of payload
     4     4  crc      u32, CRC-32C over [length | payload]
     8   len  payload  opaque bytes
```

Total record size is `8 + length`.

**Why a length prefix** rather than a delimiter: payloads are arbitrary bytes, so no byte
value can be reserved as a terminator without escaping every payload on write and unescaping
on read. A length prefix makes the record self-describing at no per-byte cost, which means
the whole log can be scanned from offset 0 to rebuild anything derived from it.

**Why the CRC covers the length field** and not just the payload: a length field covered by
nothing has to be trusted blindly. A header torn mid-write can hold a plausible but wrong
length — covering it means that shows up as a verification failure rather than as an attempt
to read a garbage number of bytes.

**Algorithm is CRC-32C** (Castagnoli, `CRC_32_ISCSI` in the `crc` crate). Same choice as
ext4, Btrfs, RocksDB and Kafka. It has a hardware instruction on x86-64 and ARM64 if
throughput ever matters; the `crc32c` crate uses it and produces identical values.

**Maximum payload size is `u32::MAX`.** Larger payloads are rejected at append with an
error — never truncated.

## Reading a record

Order matters, and it is deliberately paranoid:

1. Check the 8-byte header lies within the log.
2. Read the header, decode length and crc.
3. **Before allocating**, check that `offset + 8 + length` lies within the log. A torn
   header can claim 4 GB.
4. Read exactly `length` bytes.
5. Recompute the checksum over the header's length bytes and the payload; compare.
6. Only then return the payload.

**Steps 1 and 3 are the same arithmetic and mean different things.** Step 1 judges an
argument: the caller asked for an offset that does not address a record header inside this
log. Nothing on disk is wrong, and the error says so — an out-of-range offset, reported
with the log's length.

Step 3 judges bytes that came off the disk. The offset was valid, the header was read, and
the length it declares runs past the end of the log. The caller did nothing wrong; the
record is not trustworthy. That is reported as **corruption at the record's offset**, never
as a range error. This is the check the CRC-covers-the-length decision above exists to back
up: a header torn mid-write can hold a plausible but wrong length, and this is where that
shows up before anything is allocated.

The distinction is not cosmetic. Recovery only gets to ask "torn tail, or real damage?"
about records it has already classified as corrupt. Reporting a lying length as a range
error would drop the record out of that decision entirely, and the scan would have no way
to tell a truncated final write from a severed log.

## Recovery

Append-only means damage can only occur where writing stopped. So a record that fails
verification has two possible meanings, and they must be distinguished:

- **At the tail of the file** — a torn write. The record was never fully written, so it
  never happened. Truncate at that offset and resume writing there.
- **Anywhere else** — corruption. There are valid records after it, and their offsets are
  permanent addresses that consumers hold; they cannot be renumbered. The next record's
  position also can't be found, since the length field is itself suspect. The log is
  severed. The store refuses to open and reports the offset.

A single-node store has no way to repair the second case. Production systems recover from
a replica. Failing loudly is the honest alternative.

## Durability

**`append` syncs before it returns.** A successful append means the bytes are on stable
storage, not merely in the kernel's page cache. This is the store's central promise, and
paying for it on every append is the only default that can be trusted without being
documented.

**`sync_data`, not `sync_all`.** `sync_data` (`fdatasync`) skips metadata the store does
not care about — timestamps — but still flushes the metadata required to retrieve the
data, which for an append-only file includes the new file length. A flush that left the
length stale would put bytes past a stale EOF, which is worth nothing. On Apple targets
Rust's `sync_data` issues `fcntl(F_FULLFSYNC)`, which flushes the drive's own write cache;
a plain `fsync` there does not, and neither does `F_BARRIERFSYNC`, which only orders writes.

**The offset advances after the sync, not before.** A failed sync leaves nothing
acknowledged: `append` returns an error and the write offset still points at the last
record known to be durable. Bytes may sit in the file past that point. What happens to them
at the next open depends on how much of the record reached the disk: a complete, verifying
record is accepted into the log at the offset it already had, and an incomplete one is
truncated as a torn tail. Either outcome is consistent — the caller was told the append
failed, and offsets already handed out do not move.

**A sync failure should be treated as fatal to the store.** On Linux a failed `fsync` can
mark the error consumed, so a retry returns success while the dirty pages are already gone
— there is no way to find out afterwards which writes survived. The honest response is to
stop using the log, not to retry. *Not currently enforced:* `append` returns the error and
the `Log` remains usable.

**The cost is the point of the knob.** One fsync is hundreds of microseconds on an SSD and
milliseconds on a spinning disk, so syncing per append caps throughput at a few thousand
writes a second regardless of CPU. `sync` is public so that a caller can eventually batch —
append many records, sync once — which is the group-commit trade every production store
exposes (Postgres `synchronous_commit`, Kafka `flush.ms`). Until that exists here, the safe
default stands.

## Write path invariants

- The write offset is tracked in memory and advanced only *after* a write completes. At
  open it is not taken from the file length: the log is scanned from offset 0, every record
  verified, and the offset set to the end of the last record that passed.
- **Open may modify the file.** A torn tail is truncated at the offset where verification
  failed, so after a successful open the write offset and the physical end of the file
  always agree. Corruption is the other branch: open fails and the file is left untouched,
  because that damage is not the store's to repair.
- A record is built in a single buffer and written with one `write_all`. One syscall, and
  no crash can land between a header and its payload.
- A partial write failure leaves the file and the in-memory offset inconsistent. The store
  is unusable until reopened, where recovery truncates the mess. Not currently enforced.

## Event frame

Everything above describes the **log**, which stores opaque byte payloads and knows nothing
else. This section describes the layer above it: what the store puts *inside* a payload.
The log is unchanged by it — same 8-byte header, same CRC, same recovery rule.

```
offset  size  field
------  ----  ---------------------------------------------------
     0     2  stream_len  u16, byte length of stream_id
     2     8  version     u64, 0-based position within the stream
    10     n  stream_id   UTF-8, n = stream_len
  10+n   ...  data        opaque bytes, runs to the end of the record
```

Fixed overhead is 10 bytes per event.

**Why inside the payload rather than in the record header.** The log's contract is framing,
checksums and recovery over bytes it does not interpret; streams are a concept of the layer
above. Keeping them apart means the record header stays fixed at 8 bytes and every rule in
*Reading a record* keeps policing exactly one variable-length field. The frame is covered by
the record CRC like any other payload byte, so a corrupted stream id is caught by machinery
that already exists. The cost — a parse on every read — is reading two integers out of a
slice already in memory.

**Why the version is stored, when it is derivable.** The version of an event is its position
in that stream's list of offsets, so an index can always compute it. Writing it down anyway
makes the record self-describing, which is what lets a rebuild *assert* that a stream's
versions are contiguous instead of assuming it. An index is a derived structure; the log has
to be able to prove it wrong.

**Versions are 0-based and contiguous.** Within a stream, versions are `0, 1, 2, …` with no
gaps, ascending in log order. So the index lookup *is* the version — `offsets[n]` holds
version `n` — with no arithmetic anywhere. "The stream does not exist" is therefore not a
version number; it is the absence of the stream, and the concurrency check models it as its
own case rather than as a magic zero.

**`stream_len` is a `u16`.** Stream ids are aggregate identifiers, not documents; 64 KiB is
already absurd, and Kafka caps topic names at 249 bytes. A `u32` would write two extra zero
bytes on every event forever. A zero-length stream id is invalid in both directions: it is
rejected at encode and treated as a malformed frame at decode.

**`data` has no length prefix.** The record's own length field already bounds the payload, so
the data simply runs to the end. A second length would be a value that can disagree with the
first one — a thing to validate rather than a thing to trust. Empty `data` is legal: an event
whose occurrence is its entire meaning still has a stream and a version.

**No format tag.** A version byte at the front of the frame would be cheap insurance, but it
would only ever cover this frame. If the on-disk format changes incompatibly, the record
layout and the index are equally affected, and the honest answer is a file-level format
version decided once for the whole store — not a per-record byte that protects one of the
three things that would need to move together.

### Decoding a frame

Paranoid in the same way, and for the same reason: the bytes may be anything.

1. The payload is at least 10 bytes. Otherwise the frame is truncated.
2. Decode `stream_len` and `version`.
3. Check `10 + stream_len <= payload.len()`. A declared id longer than the payload is a
   malformed frame, checked before the id is read.
4. `stream_len` is non-zero.
5. The stream id bytes are valid UTF-8.
6. `data` is whatever remains, possibly empty.

A decoder sees a slice of bytes and does not know which record they came from, so it cannot
name an offset. It reports *why* the frame is malformed and nothing more; the caller, which
knows the offset, is what turns that into corruption reported at a position. This is the same
split as *Reading a record*: judging bytes and locating them are two different jobs.

### Worked example

Stream `orders-1`, version 3, data `hi`:

```
08 00                    stream_len = 8
03 00 00 00 00 00 00 00  version = 3
6f 72 64 65 72 73 2d 31  "orders-1"
68 69                    "hi"
```

20 bytes, which the log then stores as a 28-byte record.

## The stream index

The log answers "what is at this offset". Loading an aggregate asks the opposite question —
"where are the events of this stream" — and answering it by scanning the whole log costs
O(total events in the store) per load, on the hottest path there is. So the store keeps a
map from stream id to that stream's offsets, in log order.

**The index is derived.** It holds nothing that cannot be recomputed by replaying the log
from offset 0, which is exactly how it is built: `open` scans every record, decodes its
frame, and appends the offset to that stream's list. Nothing is persisted, so there is no
second copy to reconcile — the index either exists in memory, correct by construction, or it
does not exist at all. What a rebuild can still find is the log contradicting *itself*, and
that is not a disagreement to resolve in anyone's favour: it is damage, and open fails.

**`Vec<offset>` per stream, and the position is the version.** Versions are 0-based and
contiguous, so `offsets[n]` holds version `n` and the last version is `len() - 1`. That
subtraction is only safe because a stream's list is never empty — an entry is created only
together with the offset that justifies it. A stream with no events is not a stream with an
empty list; it is a stream that is absent from the map, which is what makes "the stream does
not exist" representable without a magic version number.

**Contiguity is asserted, not assumed.** Each record's frame carries its version, so the
rebuild can compare what the log says against the position it is about to assign. A mismatch
in either direction is rejected: a version ahead means events are missing, a version behind
means one was written twice. The index cannot be trusted to check itself, and this is the
only place the written version earns its eight bytes.

**A bad frame is corruption, never a torn tail.** A record that reaches the frame decoder has
already passed its CRC, which means its bytes are exactly the bytes handed to `write_all`. An
interrupted write cannot produce them. So a frame that fails to decode, or whose version is
not the next one for its stream, is reported as corruption at that record's offset — even
when it is the last record in the file, where a CRC failure would instead have been truncated
as a torn write. The two failures look alike at the tail and mean opposite things: one is a
write that never happened, the other is a write that happened and is wrong.

**Rebuilding at open is a cost that grows with the log.** Every open reads and decodes every
record. That is the price of having no persistent index, and it is the problem milestone 3
exists to solve; the replay path stays regardless, because a persistent index that disagrees
with the log has to be thrown away and recomputed from it.

## Reading a stream

`read_stream` answers the question the index exists for: give me the events of this
stream, in order. It reads the stream's offset list and fetches each record; nothing is
written, no invariant is new, and the on-disk format is untouched.

**An absent stream yields no events, not an error.** Every aggregate's first command loads
a stream that does not exist yet — the handler reads nothing, decides, and appends the
event that creates it. That is the birth path, not a failure. The distinction the caller
does need, "is this stream new?", belongs to the concurrency check, which models it as its
own case and reads it from the index directly. A read has no use for it, so absent and
empty behave identically here.

**The read path is lazy: it returns an iterator, not a `Vec`.** Folding an aggregate often
stops early, and a stream can be arbitrarily long; materialising every event before the
caller looks at the first one allocates work that may never be needed. Collecting into a
`Vec` stays one call away for the callers that want it.

**Laziness saves memory, not I/O.** Unlike the recovery scan, which walks the file front to
back, a stream read is one positioned read per event at scattered offsets. The iterator
avoids holding them all at once; it does not make the reads cheaper. What makes them
cheaper is fewer of them — snapshots, or a layout that keeps a stream's records together —
and neither exists here.

**The iterator borrows the store for as long as it lives.** That is not incidental: it is
the read being honest about what it is. A lazy read over a log that can still be appended
to is not a snapshot, and the borrow makes the overlap impossible to write rather than
merely wrong. A caller that wants both collects first and lets the borrow end. Production
stores face the same question and answer it with versioned reads — an iterator that sees
the log as of the moment it was opened — which costs machinery this store does not have.

**Failure is per event, not per call.** Opening verified every record, but the file is
still a file: the store does not own the only handle to it, and damage can appear after the
rebuild. So each item is a `Result` — a failed checksum or a malformed frame is reported as
corruption at that record's offset, exactly as during recovery.

**A corrupt event ends the read.** Once an item is an error the iterator yields nothing
further, and unlike the recovery scan this is a choice rather than a constraint. A record
the log cannot decode costs the scan the position of the next one, so it has nowhere to
continue to; a stream read has every offset in hand already and could skip the bad record
and carry on. It does not, because the caller folding a stream into an aggregate would then
be handed a state assembled from a stream with a hole in it. A caller that checks its
errors — collecting into a single `Result`, say — sees no difference either way. One that
discards them sees a short stream instead of a wrong answer, which is the failure worth
having.

**Contiguity is not re-checked on read.** The rebuild already asserted, against the versions
written in the frames, that the offset list is the stream in order. Re-asserting it here
would be the index verifying itself from the same data it was built from, which proves
nothing. The log is the only thing that can contradict the index, and open is where it gets
to.

## Open questions

- Whether the log stays one file or splits into segments, and what that does to offsets as
  permanent addresses.