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

## Open questions

- Where stream id and version live: a larger record header, or a framed structure inside
  the payload. The second keeps the log ignorant of what it stores, which is the stated
  goal — but costs a parse on every read.
- Whether the log stays one file or splits into segments, and what that does to offsets as
  permanent addresses.