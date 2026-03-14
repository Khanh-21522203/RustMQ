# Rust-MQ Code Review Guide

This guide helps you review the current codebase systematically against the intended design in `docs/` and `plans/`.

---

## 1) Review Goal

You are validating 3 things:

1. **Implementation correctness** — behavior matches design.
2. **Spec alignment** — code matches `plans/*.md` and `docs/*.md`.
3. **Production risk** — identify gaps, dead paths, and missing tests.

---

## 2) Fast Start (15 minutes)

Run this first to establish current baseline:

```bash
cargo test -q
cargo test -- --list
cargo fmt --check
cargo clippy --all-targets --all-features
```

What to capture:
- Any warnings/errors (especially unused/dead code warnings).
- Number of real tests vs the test strategy promised in plans.

---

## 3) Read Order (high signal first)

1. `docs/architecture.md`
2. `docs/concepts.md`
3. `docs/configuration.md`
4. `docs/api/grpc.md`
5. `src/main.rs` (actual runtime wiring)
6. `src/broker/mod.rs` and `src/client/mod.rs` (active module surface)

Then deep dive using plan files (Section 5).

---

## 4) Build a “What Is Actually Active?” map

Before judging correctness, separate active code from legacy/inactive code.

### Active runtime entry points
- `src/main.rs`
- `src/broker/mod.rs` exports
- `src/client/mod.rs` exports

### Command to detect plan/code drift

```bash
for f in plans/*.md; do
  while read -r p; do
    [ -f "$p" ] || echo "$f -> missing $p"
  done < <(rg -o 'src/[A-Za-z0-9_./-]+' "$f" | sort -u)
done
```

Use this to flag any plan that references files not present in code.

---

## 5) Feature-by-feature Review Checklist

Use each plan as the spec, then inspect implementation.

### A. Core data path (most critical)
- Plan: `plans/plan-grpc-transport.md`, `plans/plan-broker-core.md`, `plans/plan-broker-storage.md`
- Code: `src/broker/kafka_broker_server.rs`, `src/broker/core.rs`, `src/broker/storage.rs`, `src/api/kafka.proto`
- Verify:
  - RPC -> request enum -> core dispatch -> storage -> response path is consistent.
  - Offset sentinels (`-2`, `-1`) behavior is correct in fetch/list-offset paths.
  - Error mapping is specific (not only generic error code).

### B. Client behavior
- Plan: `plans/plan-producer.md`, `plans/plan-consumer.md`, `plans/plan-configuration.md`
- Code: `src/client/producer.rs`, `src/client/consumer.rs`, `src/client/config.rs`, `src/client/kafka_broker_client.rs`
- Verify:
  - Producer batching/flush logic and shutdown semantics.
  - Consumer start offset resolution order (committed -> sentinel -> fallback).
  - Auto-commit behavior and commit timing.
  - Config defaults and validation match docs.

### C. Broker mode and cluster mode wiring
- Plan: `plans/plan-cli.md`, `plans/plan-multi-broker.md`, `plans/plan-raft-consensus.md`
- Code: `src/main.rs`, `src/broker/multi_broker.rs`, `src/broker/simple_raft.rs`, `src/broker/config.rs`
- Verify:
  - Single-node mode uses `InMemoryStorage` as expected.
  - Cluster mode actually performs Raft replication as claimed (not just local state).
  - Leader checks and write rejection behavior are correct.

### D. API contract integrity
- Plan/docs: `plans/plan-grpc-transport.md`, `docs/api/grpc.md`
- Code: `src/api/kafka.proto`, generated code usage in server/client
- Verify:
  - Field names/semantics in docs match `.proto` and implementation.
  - Error code documentation matches actual enum + returned values.

---

## 6) Review for Known Drift Patterns

These are high-priority checks because they often indicate LLM-generated drift:

1. **Plan references non-existent files**
   - Example patterns to check: `src/types.rs`, `src/obs.rs`, `src/observability.rs`.
2. **Docs reference non-existent files**
   - Example: `README.md` references `ARCHITECTURE.md` but architecture docs are under `docs/`.
3. **Multiple Raft implementations co-existing**
   - Confirm which Raft path is compiled and used in runtime.
4. **Planned tests vs real tests mismatch**
   - Plans list extensive tests; verify which are actually implemented.

---

## 7) Suggested Evidence Template (use while reviewing)

For each finding, log in this format:

```md
### Finding: <short title>
- Severity: High / Medium / Low
- Plan/Doc ref: <file + section>
- Code ref: <file + function>
- Observed behavior: <what code does>
- Expected behavior: <what plan/docs say>
- Repro/Proof: <command/log/test>
- Recommendation: <minimal fix>
```

This makes later cleanup much faster.

---

## 8) Practical Review Order (half-day pass)

1. **Pass 1 (45m):** architecture + active module map + drift scan.
2. **Pass 2 (90m):** deep review of broker core/storage + producer/consumer paths.
3. **Pass 3 (45m):** cluster/raft validation + config and CLI correctness.
4. **Pass 4 (30m):** summarize findings by severity + propose fix order.

---

## 9) Exit Criteria (when your review is “done”)

You should be able to answer “yes” to all:

- Do runtime paths match the intended architecture in docs/plans?
- Are all critical data-path behaviors covered by meaningful tests?
- Are key plan promises either implemented or explicitly marked as gaps?
- Do docs reflect reality (or are mismatches clearly listed)?

If not, keep the review open and track unresolved items as explicit gaps.
