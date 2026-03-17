# Rust-MQ Code Review Guide

This guide is for reviewing the Rust-MQ codebase written by an LLM agent, from baseline checks to feature-level verification.
Each phase tells you which files to inspect, what to verify, and which LLM-generated failure patterns are most likely.

---

## How to Use This Guide

1. Work through phases in order; later checks depend on earlier assumptions.
2. For each phase, open all listed files and complete the checklist before moving on.
3. Run the verification commands in each phase and keep evidence for findings.
4. Use the **Common LLM Pitfalls** section in every phase as a focused bug-hunting map.
5. Treat `plans/*.md` and `docs/*.md` as spec intent, and code under `src/` as actual behavior.

---

## General LLM Code Patterns to Watch For (All Phases)

| Pattern | What to check |
|---|---|
| **Spec drift** | Plan/docs promise behavior that is not implemented in runtime paths |
| **Dead module references** | `mod` exports or doc references pointing to missing files |
| **Generic error mapping** | Different failures collapsed into one response code |
| **Sentinel offset mishandling** | `-2` (earliest) / `-1` (latest) interpreted incorrectly |
| **Partial shutdown logic** | Flush/commit/close paths skipped during shutdown |
| **Implicit defaults mismatch** | Runtime defaults differ from docs/config examples |
| **Stub validation** | Config validators accept almost everything |
| **Test strategy drift** | Plans mention critical tests that do not exist in code |

---

## Phase 0: Baseline and Drift Scan

**Files to review**

- `docs/architecture.md`
- `docs/concepts.md`
- `docs/configuration.md`
- `docs/api/grpc.md`
- `README.md`

**Checklist**

- [ ] Baseline checks run cleanly:
  ```bash
  cargo test -q
  cargo test -- --list
  cargo fmt --check
  cargo clippy --all-targets --all-features
  ```
- [ ] Collect warnings and dead-code signals; note anything indicating inactive or orphaned paths.
- [ ] Confirm docs point to existing files only.
- [ ] Confirm `README.md` file references match actual docs locations.
- [ ] Confirm architecture claims can be mapped to concrete runtime files in `src/`.

**Common LLM Pitfalls**

- Documentation copied forward while file layout changed.
- CI-style checks pass, but behavior-level tests are missing.
- “Architecture” section describes intended state, not implemented state.

---

## Phase 1: Runtime Wiring and Active Surface

**Files to review**

- `src/main.rs`
- `src/broker/mod.rs`
- `src/client/mod.rs`
- `src/broker/config.rs`

**Checklist**

- [ ] `main.rs` selects broker mode and client wiring exactly as documented.
- [ ] `broker/mod.rs` exports only active modules used by runtime.
- [ ] `client/mod.rs` exports match the real client API surface.
- [ ] Config loading/validation path is explicit and fails clearly on invalid input.
- [ ] Runtime mode selection (single-node vs cluster) is unambiguous and testable.

**Common LLM Pitfalls**

- Legacy modules still exported after refactors.
- Runtime branch for one mode exists but is never reachable.
- Config errors logged but not returned (startup continues with invalid state).

---

## Phase 2: Core Broker Data Path

**Files to review**

- `src/broker/kafka_broker_server.rs`
- `src/broker/core.rs`
- `src/broker/storage.rs`
- `src/api/kafka.proto`
- `plans/plan-grpc-transport.md`
- `plans/plan-broker-core.md`
- `plans/plan-broker-storage.md`

**Checklist**

- [ ] End-to-end flow is coherent: RPC request -> dispatch -> storage -> response.
- [ ] Offset sentinels (`-2`, `-1`) are implemented correctly for fetch/list-offset paths.
- [ ] Error mapping preserves failure semantics (not only a generic error code).
- [ ] Storage writes/reads/deletes preserve expected topic-partition ordering.
- [ ] `.proto` contract fields and enums match server behavior.

**Common LLM Pitfalls**

- Request enum cases mapped to wrong handlers.
- “Latest” offset computed from stale in-memory state.
- Proto docs updated, but server still uses previous semantics.

---

## Phase 3: Client Producer and Consumer Behavior

**Files to review**

- `src/client/producer.rs`
- `src/client/consumer.rs`
- `src/client/config.rs`
- `src/client/kafka_broker_client.rs`
- `plans/plan-producer.md`
- `plans/plan-consumer.md`
- `plans/plan-configuration.md`

**Checklist**

- [ ] Producer batching and flush semantics match plan/docs.
- [ ] Producer shutdown flushes pending records and surfaces failures.
- [ ] Consumer start offset resolution order is correct: committed -> sentinel -> fallback.
- [ ] Auto-commit timing is deterministic and aligned with documented behavior.
- [ ] Client config defaults and validation rules match `docs/configuration.md`.

**Common LLM Pitfalls**

- Final producer batch dropped on shutdown path.
- Consumer fallback path triggered too early, bypassing committed offsets.
- Config defaults duplicated in multiple files and drift over time.

---

## Phase 4: Cluster and Raft Claims

**Files to review**

- `src/broker/multi_broker.rs`
- `src/broker/simple_raft.rs`
- `src/main.rs`
- `plans/plan-multi-broker.md`
- `plans/plan-raft-consensus.md`

**Checklist**

- [ ] Single-node mode clearly uses local/in-memory behavior as designed.
- [ ] Cluster mode applies write gating by leader role.
- [ ] Replication path is real (not only local write + success response).
- [ ] Raft implementation in runtime is the same one described in plans.
- [ ] Failure and re-election behavior are represented in tests or explicit gaps.

**Common LLM Pitfalls**

- Multiple raft-like modules exist; runtime uses a different one than docs describe.
- Leader checks applied in one write path but missing in another.
- Replication “success” returned before durable apply/ack path completes.

---

## Phase 5: Plan-to-Code Drift Audit

**Files to review**

- `plans/*.md`
- `docs/*.md`
- `src/**/*.rs`

**Checklist**

- [ ] Run a plan/code reference scan and flag missing files:
  ```bash
  for f in plans/*.md; do
    while read -r p; do
      [ -f "$p" ] || echo "$f -> missing $p"
    done < <(rg -o 'src/[A-Za-z0-9_./-]+' "$f" | sort -u)
  done
  ```
- [ ] For each missing reference, classify: renamed file, dropped feature, or stale plan text.
- [ ] Verify planned tests have corresponding real tests.
- [ ] Record unresolved gaps as explicit backlog items.

**Common LLM Pitfalls**

- Plans reference files that never existed in this repository version.
- Test plans are detailed, but only smoke tests were implemented.
- Gaps identified during review are not tracked, then regress in later phases.

---

## Evidence Template for Findings

Use this format for each issue:

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

---

## Exit Criteria

Do not close the review until all are true:

- Runtime behavior matches the documented architecture and plan intent.
- Critical broker and client data paths have meaningful test coverage.
- Cluster/raft claims are either proven by code/tests or logged as gaps.
- Documentation mismatches are either fixed or explicitly tracked.
