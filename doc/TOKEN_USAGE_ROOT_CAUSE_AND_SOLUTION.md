# Token usage stays at zero: root cause and proposed solution

Date: 2026-11-17

Source revision: `21ac11bf8918c5cc8ffb4950e32ec4732a0154c6`

Scope: investigation and solution design only; no application source changes.

## Conclusion

**The Copilot ACP adapter reads the wrong shape for `usage_update.used`.**
ACP defines `used` as an integer, but the application treats it as an object
containing input/output/cache counters. A valid update such as `used: 12800`
therefore becomes `Some(0)`, is saved as zero, and displays as `0.0k`.
Completing the prompt, task, or process does not repair that value.

There is also a semantic problem: ACP's `used` means **tokens currently in the
context window**, not cumulative tokens consumed by a task/session. Merely
parsing the integer into the existing field would stop the false zero but
would not make the current consumption-oriented labels accurate.

This defect is reproduced against the actual Rust parser and database writer,
using synthetic, protocol-valid messages and an in-memory SQLite database.
No affected user's live ACP stream or application database was inspected.
Consequently, the code defect is confirmed, but whether a particular installed
provider emits these updates, omits usage entirely, or uses another extension
still requires a narrowly scoped, redacted protocol capture.

## Evidence and data flow

| Stage | Current behavior | Source |
| --- | --- | --- |
| Session creation | `token_count INTEGER NOT NULL DEFAULT 0`; unknown usage initially looks like measured zero. | [lib.rs:3757](../src-tauri/src/lib.rs#L3757) |
| Provider transport | Copilot runs with `--acp`; stdout messages are parsed as JSON. | [lib.rs:2120-2215](../src-tauri/src/lib.rs#L2120-L2215) |
| Usage parsing | Only recognizes `sessionUpdate == "usage_update"`; looks for `used.inputTokens`, `used.outputTokens`, `used.cacheReadInputTokens`, and `used.cacheCreationInputTokens`. | [lib.rs:864-882](../src-tauri/src/lib.rs#L864-L882) |
| Empty sum | Numeric `used` has none of those fields. Each lookup becomes `None`; flattening and summing from zero returns `Some(0)`, not an unsupported/malformed signal. | Same parser |
| Persistence | The reader writes that value; SQL replaces the existing count with `SET token_count = ?2`. The reader discards persistence errors with `let _ = ...`. | [lib.rs:884-901](../src-tauri/src/lib.rs#L884-L901), [lib.rs:2217-2220](../src-tauri/src/lib.rs#L2217-L2220) |
| Read model | Queries persisted `s.token_count` and formats it as one-decimal thousands. | [lib.rs:1748-1795](../src-tauri/src/lib.rs#L1748-L1795), [lib.rs:240-253](../src-tauri/src/lib.rs#L240-L253) |
| Display | Session details render `selectedSession.tokens`; update events reload sessions. | [App.tsx:553-557](../src/App.tsx#L553-L557), [App.tsx:858](../src/App.tsx#L858) |

The valid notification shape is:

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "fixture",
    "update": {
      "sessionUpdate": "usage_update",
      "used": 12800,
      "size": 128000
    }
  }
}
```

The latest published ACP JSON schema was checked during this investigation:
`UsageUpdate.used` and `.size` are required nonnegative integers. The protocol
defines them as current context tokens and total context-window size.
Usage reporting itself is optional.

### Why waiting for completion does not help

1. The ACP reader routes responses with numeric request IDs to the pending
   request and continues; they do not pass through the normal notification
   observation path ([lib.rs:2339-2346](../src-tauri/src/lib.rs#L2339-L2346)).
2. `send_copilot_message` receives the `session/prompt` result but matches
   `Ok(_)`, discarding the body before updating steps/status and handling task
   completion ([lib.rs:1004-1031](../src-tauri/src/lib.rs#L1004-L1031)).
3. Task completion interprets `apt-status` and updates task/session lifecycle,
   not token accounting ([lib.rs:1570-1641](../src-tauri/src/lib.rs#L1570-L1641),
   [project_features.rs:877-935](../src-tauri/src/project_features.rs#L877-L935)).
   Process exit also updates lifecycle without requesting final usage
   ([lib.rs:2406-2452](../src-tauri/src/lib.rs#L2406-L2452)).

Thus a malformed parse can keep writing zero; if there is no usage notification,
the original default zero simply remains. An assistant's `apt-status` completion
report is not authoritative provider token telemetry.

Do not assume that `result.usage` is guaranteed to solve this: ACP's
[end-turn token usage proposal](https://agentclientprotocol.com/rfds/end-turn-token-usage.md)
is still **Draft** at the time checked. Its optional `result.usage.totalTokens`
shape is a proposed interface, not a stable requirement. If an actual supported
Copilot version emits an extension there, the current code ignores it, but that
is a conditional additional gap, not evidence that the affected session did so.

## Reproduction and hypothesis checks

A temporary harness extracted the unmodified
`copilot_token_count_from_message` and `update_session_token_count` functions
from this checkout, compiled them against existing Rust build dependencies,
and replayed messages into an in-memory database. It made no provider calls
and did not open any real session database.

The initial assertion expected a nonzero valid update to persist `12800`.
It failed with exit code `101`:

```text
numeric ACP used: parsed=Some(0), persisted=0, display=0.0k
assertion failed: nonzero ACP usage must not display zero
  left: Some(0)
 right: Some(12800)
```

Additional cases, run twice with identical results:

| Input | Parser output | Stored/displayed result | Interpretation |
| --- | --- | --- | --- |
| Valid numeric `used: 12800` | `Some(0)` | `0` / `0.0k` | Confirmed primary defect |
| Synthetic object `used: {inputTokens: 10000, outputTokens: 2800}` | `Some(12800)` | `12800` / `12.8k` | Positive control: proves parser shape mismatch, not a broken SQLite writer |
| `usage_update` missing `used`, after the positive control | `Some(0)` | `0` / `0.0k` | Malformed/missing data can erase an earlier valid value |
| Synthetic draft-shaped `result.usage` | `None` | Previous value unchanged | This parser does not consume response usage; not proof the provider emits it |

Ranked hypotheses and their distinguishing predictions:

| Hypothesis | Prediction | Result |
| --- | --- | --- |
| 1. Wrong payload shape | Numeric `used` yields zero while the object-shaped control yields nonzero. | Confirmed by replay |
| 2. Completion loses/omits accounting | A completion-only usage object cannot update this counter; lifecycle completion has no independent accounting step. | Confirmed code path; actual provider response availability unverified |
| 3. UI rounding or refresh causes all zeros | A persisted count of `12800` would still render zero. | Rejected as the primary cause by the existing read-model test; UI subscribes to update events |

One-decimal thousands formatting can independently show counts below 50 as
`0.0k`, but cannot explain a parsed/stored `12800` becoming zero. Also,
the summary card sums already-rounded display strings and labels the result
**"Tokens this hour"**, without any hourly usage-event filter
([App.tsx:808](../src/App.tsx#L808)). That is a related metric-design issue,
not the reason the backend records zero.

### Provider scope

The confirmed payload mismatch is specific to the Copilot ACP path. Claude
Code has a separate parser: on a `result` event it sums snake_case counters
from `usage`, then writes them on completion
([lib.rs:598-616](../src-tauri/src/lib.rs#L598-L616),
[lib.rs:3471-3482](../src-tauri/src/lib.rs#L3471-L3482)).
Its existing test correctly reports `10 + 5 = 15`.

Do not claim all Claude sessions necessarily exhibit the same defect. Missing
Claude usage also sums to zero, and writes replace rather than accumulate;
multi-turn semantics and missing-data handling need explicit provider fixtures.

## Proposed solution

### 1. Correct the Copilot protocol boundary and label the metric honestly

In Rust, validate the notification method, update discriminator, and session
identity. Decode numeric `used` and `size` into checked integer fields using
the published ACP contract. Reject negative, fractional, wrongly typed, and
out-of-range values without overwriting the last valid measurement. Do not
silently convert malformed/missing usage to zero. A real, valid `used: 0`
must remain distinguishable from unavailable data.

Persist these as **context tokens used / context-window size**, with observation
time and source, and show them as "Context tokens" or "Context window" in the UI.
Treat each update as a snapshot: replace it, do not add it. A context count may
decrease after compaction; that is not negative consumption or lost accounting.
Do not reuse it as a billed-token or cumulative-consumption counter.

Do not implement speculative object-field aliases. Support a legacy/extension
shape only if a real, version-identified provider fixture establishes its
contract and semantics.

### 2. Represent unknown usage explicitly

Use nullable measurements or an explicit availability state across SQLite,
Rust DTOs, and TypeScript types. Render "Not reported" when a supported provider
does not supply a metric, and expose malformed data or persistence failures
through the existing user-visible error/event paths.

Do not discard `update_session_token_count` failures. Commit measurements before
emitting a successful data-change notification; retain an explicit error
condition if persistence fails. A telemetry error should not falsely claim
that the agent's work itself failed.

Expose raw numeric values to React and format only at rendering time. Show
small counts as integers so positive measurements cannot visually round to
zero. Do not aggregate formatted strings.

### 3. Add consumed-token accounting only from an authoritative source

For the original "tokens consumed by session/task" requirement, first establish
which supported provider/version actually supplies this measurement:

| Available signal | Correct action |
| --- | --- |
| Stable ACP context snapshot | Update context metric only |
| Verified provider-specific per-turn consumption | Persist a turn record and add it once to session consumption |
| Verified cumulative consumption snapshot | Replace the corresponding cumulative consumption value; never sum repeated snapshots |
| No authoritative consumption telemetry | Display "Not reported"; do not substitute context size or estimate from transcript text |

Capture only allowlisted usage fields, method/discriminator, version, and
request/turn identity at the adapter boundary. Exclude credentials, prompt
content, tool output, and private provider databases.

If a verified extension uses `session/prompt` response usage, consume the response
body instead of `Ok(_)`, normalize it in the provider adapter, and persist it
before marking the turn complete. Do not upgrade to ACP v2 or depend on a Draft
field just to fix the stable v1 context parser.

For per-turn accounting, deduplicate using a durable local turn/request identity
that remains unique across process restarts; JSON-RPC request numbers alone
restart with the process. Define input/output/cache inclusion from the provider
contract to prevent double counting. Keep Claude normalization separate and
verify whether its result counters describe a turn/invocation or a session.

Store timestamped consumption records if retaining "Tokens this hour"; query
the actual last-hour records rather than summing session lifetime snapshots.
Otherwise remove/relabel that summary. If displaying a task total across
retries, retain durable task-to-attempt/session associations: the current task
has a single `session_id` and failure can clear it
([project_features.rs:220-239](../src-tauri/src/project_features.rs#L220-L239),
[project_features.rs:914-922](../src-tauri/src/project_features.rs#L914-L922)).

### 4. Migrate without inventing historical numbers

Add context/consumption fields and availability metadata through the existing
idempotent SQLite migration approach. Existing zeros cannot distinguish
"genuinely zero" from "never reported" or "incorrectly parsed": mark their
availability as unknown unless independent app-owned evidence proves otherwise.
Do not reinterpret historical Copilot values as consumed tokens.

The Copilot usage path does not currently append the raw usage notification
to `session_events`: it updates the counter and emits a frontend event, while
the general observation parser ignores `usage_update`. Historical accurate
counts therefore cannot be assumed recoverable from the app's event table.
Backfill only from verified retained, app-owned usage evidence if any exists.
Otherwise leave old measurements unavailable; new telemetry can populate new
measurements, not reconstruct past consumption.

## Implementation acceptance criteria

These are **future fix gates**, not claims that a fix was implemented here:

| Case | Required behavior |
| --- | --- |
| Numeric context update: `used: 12800`, `size: 128000` | Persist/display `12.8k` context tokens and 10% context utilization |
| Valid zero versus missing/malformed usage | Valid zero displays zero; missing data is unavailable; malformed data reports a diagnostic and does not erase valid data |
| Negative/fractional/overflow, wrong method/session | No corrupt write; explicit validation diagnostics for malformed relevant messages |
| Duplicate snapshot and compaction | No double counting; a smaller later context snapshot is accepted |
| Verified per-turn consumption, if supported | Adds once per durable turn; two turns of 100 and 200 yield 300 after replay/reconnect as well |
| Draft/unsupported completion usage | Does not become an implicit required contract; unsupported consumption remains unavailable |
| Persistence failure | Visible telemetry error; no success-shaped update hiding an unsaved measurement |
| End-to-end completed task/session | Actual adapter input reaches SQLite, typed read model, and UI; completion preserves the last valid measurement |
| Small positive measurement, e.g. 15 | Displays `15`, not `0.0k` |
| Hourly summary, if retained | Includes only consumption events in the last hour; sums raw numbers |
| Existing history/restart | No fabricated backfill, inflated totals, or loss of availability metadata |

Add focused regression tests at the real parser/writer and provider-reader
seams; retain the existing read-model and completed-task UI tests. A mocked UI
count alone would not have caught the primary defect.

## Checks performed for this investigation

| Check | Result |
| --- | --- |
| Actual Rust parser + SQLite writer replay | Expected red assertion reproduced the exact nonzero-input/zero-output defect |
| Bug-expectation replay, twice | Passed identically; positive control persisted `12800`; missing usage reset it to zero |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib claude_result_events_report_session_usage_and_failures --offline --quiet` | 1 passed |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib session_read_model_returns_persisted_copilot_session --offline --quiet` | 1 passed, including `12800` -> `12.8k` |
| `npm test -- src/App.test.tsx -t 'shows completed and failed sessions in Task outcomes with working filters'` | 1 passed; uses mocked session DTOs, not a live provider |

The red replay is deliberate evidence of the existing defect, not a passing
post-fix regression test. Source fixes and their acceptance gates are deferred
as requested. No live CLI task was launched, no credentials or private session
stores were read, and no instrumentation was added to application code.

## Protocol references

- [Published ACP JSON schema](https://github.com/agentclientprotocol/agent-client-protocol/releases/latest/download/schema.json):
  `UsageUpdate.used` and `size` integer definitions.
- [Session context size and cost](https://agentclientprotocol.com/rfds/session-usage.md):
  notification example, snapshot semantics, and optional reporting.
- [Session usage stabilization announcement](https://agentclientprotocol.com/announcements/session-usage-stabilized.md):
  stable since June 5, 2026.
- [End-turn token usage](https://agentclientprotocol.com/rfds/end-turn-token-usage.md):
  Draft status and unresolved consumption-accounting semantics.

External references were read on 2026-11-17; the `latest` schema URL is mutable.

## Appendix: standalone reproduction without source edits

Run from the repository root with Python, Rust, and the checkout's existing
compiled `serde_json`/`rusqlite` dependencies under
`src-tauri/target/debug/deps`. The targeted Cargo checks above produce these
dependencies if needed. This extracts the actual functions, not a Python
translation. Generated files live in a temporary directory and are removed
automatically.

The command below asserts the correct result and therefore **fails on the
current code** with `left: 0`, `right: 12800`. To verify the documented defect
as a diagnostic pass, change only the harness assertion's expected value to
`0`; this does not fix the application.

```bash
python3 - <<'PY'
from pathlib import Path
import subprocess
import tempfile

root = Path.cwd()
source = (root / "src-tauri/src/lib.rs").read_text()
deps = root / "src-tauri/target/debug/deps"

def extract(name):
    start = source.index(f"fn {name}(")
    return source[start:source.index("\n}\n", start) + 3]

program = (
    "use serde_json::{json, Value};\n"
    "use rusqlite::{params, Connection};\n"
    "use std::sync::{Arc, Mutex};\n"
    + extract("copilot_token_count_from_message")
    + extract("update_session_token_count")
    + r'''
fn main() {
    let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
    db.lock().unwrap().execute_batch(
        "CREATE TABLE agent_sessions (
            id TEXT PRIMARY KEY, token_count INTEGER NOT NULL DEFAULT 0,
            last_seen_at TEXT, updated_at TEXT);
         INSERT INTO agent_sessions(id) VALUES ('fixture');"
    ).unwrap();
    let message = json!({
        "jsonrpc":"2.0", "method":"session/update",
        "params":{"sessionId":"fixture","update":{
            "sessionUpdate":"usage_update","used":12800,"size":128000
        }}
    });
    let parsed = copilot_token_count_from_message(&message);
    if let Some(count) = parsed {
        update_session_token_count(&db, "fixture", count).unwrap();
    }
    let actual: i64 = db.lock().unwrap().query_row(
        "SELECT token_count FROM agent_sessions WHERE id = 'fixture'",
        [], |row| row.get(0)
    ).unwrap();
    println!("parsed={parsed:?}, persisted={actual}, display={:.1}k",
             actual as f64 / 1000.0);
    assert_eq!(actual, 12800, "nonzero ACP usage must not display zero");
}
'''
)
with tempfile.TemporaryDirectory(prefix="acp-token-probe-") as directory:
    directory = Path(directory)
    entry, binary = directory / "probe.rs", directory / "probe"
    entry.write_text(program)
    command = ["rustc", "--edition=2024", str(entry),
               "-L", f"dependency={deps}", "-o", str(binary)]
    for crate in ("serde_json", "rusqlite"):
        libraries = sorted(deps.glob(f"lib{crate}-*.rlib"))
        if not libraries:
            raise SystemExit(f"Missing compiled dependency: {crate}")
        command.extend(["--extern", f"{crate}={libraries[0]}"])
    subprocess.run(command, check=True)
    result = subprocess.run([str(binary)])
    raise SystemExit(result.returncode)
PY
```
