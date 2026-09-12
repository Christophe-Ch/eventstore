# eventstore

A single-node, embedded event store, written from scratch in Rust.

Crash-safe append-only storage, streams with optimistic concurrency, and two read paths:
one stream at a time to rebuild an aggregate, or global order to feed projections.

**This is a learning project.** It is not a library anyone should depend on, and it is not
trying to be one. The goal is to learn Rust and storage internals by building the thing
rather than reading about it.

## Status

Milestone 1a: the record log frames payloads on disk and detects corruption.
Nothing above the log exists yet — no events, no streams, no versions.

See [ROADMAP.md](ROADMAP.md) for what's built and what's next.

## What it does

At the bottom is an append-only log of opaque byte records:

```rust
let mut log = Log::open("events.log")?;
let offset = log.append(b"some payload")?;
let payload = log.read_at(offset)?;
```

A record is a fixed-size header followed by a variable-length payload. The header carries
the payload length and a CRC-32C covering both the length field and the payload, so a
record torn by a crash or damaged on disk fails verification instead of being handed back
as garbage. All integers are little-endian.

The exact layout and the recovery rules are in [DESIGN.md](DESIGN.md), which is the
authoritative spec — changing the format means changing that file first.

On top of this the store will grow streams (`account-1234`), per-stream versions, and
conditional appends, so two concurrent writers can't both approve a debit the balance
can't cover. That's milestone 2.

## Why append-only

Events are never updated or deleted, which means damage can only ever occur at the point
where writing stopped. Recovery is then well defined: a record that fails verification at
the tail of the file was never fully written, so it's truncated. The same failure anywhere
else means the log is severed, and the store refuses to open rather than pretending.

That constraint is also what makes replay possible. Read models are derived from the log
and are disposable — delete one, replay from position zero, and it rebuilds itself.

## Building

```sh
cargo test
```

No runtime dependencies beyond `crc`. `tempfile` is a dev-dependency.

Unix only. Reads use `pread` via `std::os::unix::fs::FileExt`, which has no portable
equivalent in `std`.

## Layout

```
src/lib.rs      public API
src/log.rs      the record log — framing, append, read_at
src/record.rs   record format, checksum
tests/          integration tests, public API only
```

Unit tests live next to the code they test, since most of what's here is internal
machinery. `tests/` is for scenarios describable in a sentence about the product.

## Not in scope

Networking, authentication, clustering, replication. Update and delete — the only write
operation is append. Any knowledge of what payloads mean: the store does not parse,
validate, or index by content.