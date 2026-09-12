# Roadmap

A single-node, embedded event store. Crash-safe append-only storage, streams with
optimistic concurrency, and two read paths: per-stream and global.

Milestones after 1 are sketches. Expect them to change once the earlier ones are built.

---

## 1. The record log

The log stores opaque byte payloads. It knows nothing about events, streams or versions.

- [x] **1a. Framing**
  `append(payload) -> offset`, `read_at(offset) -> payload`.
  Record layout, CRC verification, corruption detected in the length, CRC and payload fields.
- [ ] **1b. `StoreError`**
  Replace stringly-typed `io::Error` with an enum: `Io`, `Corrupt { offset, reason }`,
  `OffsetOutOfRange`, `PayloadTooLarge`. Tests assert on the variant instead of `is_err()`.
- [ ] **1c. `iter` — recovery**
  Scan from offset 0, yielding payloads until the file ends.
  Implements the recovery rule: a record that fails verification at the *tail* is a torn
  write (truncate, resume there); the same failure *anywhere else* is corruption (refuse to open).
  First hand-written `Iterator`, first borrow that outlives the method call.
- [ ] **1d. Durability**
  `sync_data` and when to call it. Only now does "append returned Ok" actually mean
  "survives power loss", and only now is the crash test meaningful.

## 2. Streams

- [ ] **2a. Stream id and version on each record**
  Either the header grows, or the payload gains its own framed structure. Decide which.
- [ ] **2b. In-memory index**
  `stream -> Vec<offset>`, rebuilt by scanning the whole log at open.
- [ ] **2c. `read_stream` and `ExpectedVersion`**
  `append(stream, expected, events) -> Result<Version, WrongVersion>`.
  Batch append must be atomic: all events visible or none.
  The concurrency guard exists from here on.

## 3. Persistent index

- [ ] **3a. Index on disk**
  Opening no longer rescans the log. Now two files must agree, and a crash can land between them.
- [ ] **3b. Rebuild from log**
  The recovery path when the index and log disagree. The log stays the source of truth.
- [ ] **3c. A real on-disk structure**
  Sorted file, or a B-tree. This is where the DBMS-from-scratch work and this project merge.

## 4. The global read path

- [ ] **4a. `read_all(from_position)`**
  Total order across all streams.
- [ ] **4b. Checkpoint storage**
  A consumer durably records the position it has processed up to.
- [ ] **4c. Toy projection**
  Integration test: fold a bank account from its stream, run a balances projection over
  `read_all`, kill it mid-run, confirm it resumes from its checkpoint.

## 5. Open questions

Pick whatever you're curious about by then:

- Group commit — batch N appends per fsync, measure the throughput curve.
- Log segmentation into multiple files, and what that does to offsets.
- Live subscriptions: block until new events arrive instead of polling.
- Concurrent readers alongside a single writer.
- Snapshots, so loading an aggregate with 100k events doesn't read all of them.

---

## Out of scope

Deliberately, and permanently for this project:

- Networking, authentication, clustering, replication.
- Update and delete. The only write operation is append.
- Any knowledge of what payloads mean. No parsing, no validation, no indexing by content.

## Notes

Milestones 1 and 2 are the project. 3 and 4 turn it into a system. 5 is optional.

1d and 3 are where this kind of project usually stalls — measuring fsync properly is
fiddly, and persistent indexes are genuinely hard. Reaching 2c with passing tests is
already a real piece of storage engine.