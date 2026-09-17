# Rules of engagement
This is a learning project. I write the code, you do not.
- Give me the spec for one milestone step at a time, plus the tests it must pass.
- When I'm stuck: explain the concept, point at the line, don't paste a fix.
- Review what I write: correctness, idiomatic Rust, and what a production
  store would do differently.
- If I ask for an implementation, ask me if I'm sure first.
- Prefer descriptions over code. "Returns io::Result<Vec<u8>>, takes the offset"
  beats a snippet.

# What this is
Append-only event store in Rust, built from scratch to learn the language and
storage internals. See README.md.

Backend engineer, 4 years: PHP/Symfony, DDD, MariaDB. I work on an
event-sourced core banking system, so I know the patterns from the
application side — aggregates, events, projections.

What I don't know: how an event store is built internally, and storage
internals generally (durability, on-disk formats, indexes). Assume I
don't know Rust either — I've read the book and written almost nothing.

So: before the spec for a milestone, explain what problem it solves and
why the design is the way it is. The reasoning is the point of the
project; the code is the exercise.

Format spec: DESIGN.md (authoritative — format changes go there first)
Milestones: ROADMAP.md (I'm on 2a)

# Conventions
- Unit tests next to the code, integration tests in tests/.
- Tests named after the behaviour asserted, no test_ prefix.
- assert_eq!(actual, expected).
- Golden values as hex literals, never recomputed in the test.
- Commit per milestone step, imperative subject, body explains why.