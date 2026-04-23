# Rust Rewrite Scaffold

This repository now contains a Rust workspace for the next-generation rewrite of `spotify-dl-gui`.

The legacy Python app remains unchanged in `spotifydl_gui/`.

## Workspace layout

- `crates/spotifydl-protocol`: shared typed commands, events, job models, snapshots
- `crates/spotifydl-core`: fake downloader backend and backend-facing abstractions
- `crates/spotifydl-storage`: SQLite persistence for queue/history/settings/logs
- `crates/spotifydl-service`: queue state machine and protocol-driven service API
- `crates/spotifydl-gui`: `iced` desktop shell wired to the fake service
- `crates/spotifydl-cli`: minimal CLI smoke-test entrypoint

## Run the GUI

```powershell
cargo run -p spotifydl-gui
```

The scaffold stores its SQLite state in the platform app-data directory under `spotifydl-rust`.

## Run the CLI

```powershell
cargo run -p spotifydl-cli
```

Useful smoke-test variants:

```powershell
cargo run -p spotifydl-cli -- status
cargo run -p spotifydl-cli -- status --json
cargo run -p spotifydl-cli -- status --require-ready
```

## Notes

- The current milestone can run either the fake backend or the external `spotify-dl` adapter.
- The external adapter now performs backend preflight checks before starting work.
- Queue/history/settings/logs are persisted in SQLite.

## Bundled Downloader Layout

For packaged builds, the Rust app should not rely on `PATH` alone to find the external
downloader.

The external backend resolves the downloader in this order:

1. explicit configured path, if one is saved in settings
2. named executable from `PATH`
3. bundled executable next to the app binary
4. bundled executable in `bin/` next to the app binary
5. bundled executable in `tools/` next to the app binary

Expected packaged layout examples:

```text
MyApp/
  spotifydl-gui.exe
  spotify-dl.exe
```

```text
MyApp/
  spotifydl-gui.exe
  bin/
    spotify-dl.exe
```

```text
MyApp/
  spotifydl-gui.exe
  tools/
    spotify-dl.exe
```

Recommended release smoke test:

```powershell
.\spotifydl-cli.exe status --require-ready
```

Repository helper for staging and validating a Rust package directory:

```powershell
.\scripts\validate-rust-package.ps1
```

That script will:

- build `spotifydl-cli.exe` and `spotifydl-gui.exe` in release mode
- create `dist\rust-package\`
- copy the Rust CLI and GUI into that directory
- copy `spotify-dl.exe` into the packaged root
- run `spotifydl-cli.exe status --require-ready` against a package-local SQLite file

Useful variants:

```powershell
.\scripts\validate-rust-package.ps1 -DownloaderBinary path\to\spotify-dl.exe
.\scripts\validate-rust-package.ps1 -JsonStatus
.\scripts\validate-rust-package.ps1 -SkipBuild -CliBinary target\debug\spotifydl-cli.exe -GuiBinary target\debug\spotifydl-gui.exe
```

That should confirm:

- the SQLite database opens
- the selected backend is visible
- the external downloader is actually discoverable from the packaged layout

## Architecture Lock

This rewrite is intended to be a layered Rust app, not a GUI bolted onto the existing
CLI logic.

### Crate responsibilities

- `crates/spotifydl-protocol`
  - shared types only
  - commands, events, queue/job/item state, settings, history, outputs
- `crates/spotifydl-core`
  - downloader-facing logic
  - eventually wraps the modified `spotify-dl`
  - no GUI concerns
- `crates/spotifydl-storage`
  - SQLite persistence
  - queue, history, settings, logs
- `crates/spotifydl-service`
  - queue state machine
  - orchestration between GUI, storage, and core
- `crates/spotifydl-gui`
  - `iced` UI only
  - renders snapshots and dispatches commands
- `crates/spotifydl-cli`
  - optional smoke-test/admin tool over the same protocol/service

### Non-negotiable rules

- The GUI must not parse logs to determine downloader state.
- The GUI must not diff the filesystem to infer outputs.
- The downloader/service layers must emit typed events for all user-visible state.
- The GUI must depend on `spotifydl-protocol`, not on `spotifydl-core`.
- The existing downloader implementation in `https://github.com/z-er/spotify-dl` should be
  adapted or embedded through `spotifydl-core`, not rewritten in the GUI repo.

## Step 2: Target Protocol Contract

The current scaffold protocol is intentionally small and fake. The long-term contract
should move toward the shape below.

### Commands

These are the service commands the GUI should eventually rely on:

- `EnqueueUrls`
  - add one or more Spotify URLs as a job
- `AddUrlsToJob`
  - append URLs to an existing job
- `StartQueue`
  - start processing the queue
- `PauseQueue`
  - pause after the current active boundary
- `ResumeQueue`
  - resume a paused queue
- `CancelJob`
  - cancel an entire job
- `RetryFailedItems`
  - reset failed or cancelled items to pending
- `RemoveJob`
  - remove a non-running job from the queue/history
- `ReorderJob`
  - move a job in the queue
- `ClearHistory`
  - clear persisted run history
- `UpdateSettings`
  - write settings atomically
- `RequestSnapshot`
  - request a fresh full application snapshot
- `AcknowledgeError`
  - dismiss non-fatal surfaced service errors

### Events

These are the events the service/core layers should emit:

- `SnapshotUpdated`
  - full state snapshot after meaningful changes
- `QueueStatusChanged`
  - queue entered idle/running/paused/backoff state
- `JobQueued`
  - a job was created
- `JobUpdated`
  - a job changed materially
- `JobRemoved`
  - a job was removed from queue/history
- `JobStarted`
  - a job became active
- `JobFinished`
  - a job reached success/failed/cancelled
- `ItemStarted`
  - a specific item started
- `ItemProgress`
  - progress/state update for one item
- `ItemFinished`
  - final result for one item
- `BackoffStarted`
  - service/backend entered cooldown
- `BackoffTick`
  - cooldown countdown tick
- `LogAppended`
  - append-only log entry for diagnostics
- `HistoryUpdated`
  - history list changed
- `SettingsUpdated`
  - settings were persisted
- `ErrorRaised`
  - structured non-fatal error surfaced to UI

### Core Models

The stable shared data model should include:

- `QueueState`
  - status, active job id, queued jobs
- `JobRecord`
  - id, label, source, state, options, timestamps, progress, totals, items
- `JobItemRecord`
  - id, url, state, progress, metadata, final result
- `AppSnapshot`
  - queue, history, logs, settings, service health
- `HistoryEntry`
  - finished job summary plus original inputs
- `AppSettings`
  - destination, format, concurrency, organizer/integrity/web/sentry settings
- `LogEntry`
  - timestamp, level, scope, job/item ids, message

### Downloader Result Models

These should come from the core/service layers directly, not be inferred by the GUI:

- `OutputRecord`
  - final path, artist, album, title, size, format
- `DuplicateDecision`
  - replaced, skipped, kept_both, deleted_smaller
- `IntegrityFlag`
  - size/duration checks and reason
- `FailureReason`
  - cancelled, auth, rate_limit, network, io, metadata, organizer, unknown
- `BackoffState`
  - active flag, delay ms, remaining ms, reason

### Immediate Implementation Goal

The next code step should be:

1. Refactor `spotifydl-protocol` to match the target command/event/model split.
2. Keep serialization simple and stable with `serde`.
3. Avoid introducing downloader-specific internals into the GUI crate.
4. Update `spotifydl-service` to compile against the richer protocol types.

The real downloader integration should happen only after the protocol/service boundary
is stable enough to survive backend changes without GUI rewrites.

## Rewrite Milestones

The remaining work is best treated as a staged rewrite rather than one large merge.

### M0: Foundation Baseline

Status:
- mostly done

Exit criteria:
- workspace compiles
- protocol/service/storage/core boundaries are stable enough to iterate
- fake backend drives GUI/CLI for smoke testing

Notes:
- this is the current state of the Rust rewrite
- the app is architecturally credible, but not operational as a real downloader yet

### M1: Real Downloader Adapter Boundary

Status:
- mostly done

Scope:
- adapt the external `z-er/spotify-dl` implementation through `crates/spotifydl-core`
- define how jobs/options/items map to downloader invocation
- define how downloader output/logs/structured data become backend events
- keep the fake backend as a fallback test backend

Exit criteria:
- `spotifydl-core` has a real adapter implementation path, not just a fake backend
- command planning / process execution / event decoding are explicit responsibilities
- failures, outputs, item completion, and backoff can originate in the adapter

Current state:
- external process execution exists in `spotifydl-core`
- upstream JSON event decoding is wired into backend events
- packaged-layout executable discovery exists
- fake backend remains available for tests and smoke usage

### M2: Service Hardening Against Real Runs

Status:
- in progress

Scope:
- validate queue state transitions against actual backend behavior
- implement robust retry/cancel/recovery semantics
- handle partial success, auth failures, rate limiting, and restart recovery
- tighten history/log/error mapping for real runs

Exit criteria:
- queue state remains correct under real downloader failures and interruptions
- persisted jobs recover into truthful paused/recoverable states
- service tests cover the main success and failure paths

Current state:
- service consumes structured backend events instead of toy progress-only updates
- partial success, skip flags, backoff, requeue, backend preflight, and boundary pause are covered by tests
- backend readiness is surfaced into persisted service health
- reopen recovery is covered for persisted active runs and persisted active backoff state

Remaining high-value work:
- restart/interruption truthfulness against real external runs
- tighter cancel semantics and recovery expectations around abrupt process termination
- more end-to-end validation against the actual external adapter instead of only scripted backends

### M3: Usable GUI Parity

Status:
- in progress

Scope:
- settings UI
- item-level detail and output/flag/failure views
- better queue controls and history workflows
- clearer surfaced errors and service health state

Exit criteria:
- a user can configure, enqueue, run, inspect, retry, cancel, and review history from the GUI
- GUI depends only on protocol/service state, not backend internals

Current state:
- backend selection and executable path are configurable
- default destination / format / parallelism are configurable
- queue and history show item-level outputs, flags, failures, and totals
- history jobs can be requeued from the GUI
- backend readiness and status are visible in the GUI

Remaining high-value work:
- more polished settings UX and validation feedback
- clearer retry/cancel/details workflows
- richer operator-facing surfacing for backoff, recovery, and external backend errors

### M4: Release Readiness

Status:
- in progress

Scope:
- storage migrations and compatibility checks
- packaging/distribution
- operational logging policy
- smoke tests and end-to-end validation

Exit criteria:
- reproducible builds exist for target platforms
- migrations are safe across app upgrades
- the Rust app is realistic to ship as the primary implementation

Current state:
- backend readiness is validated before runs start
- bundled-downloader discovery is implemented
- `spotifydl-cli status --require-ready` exists as a smoke path
- `scripts/validate-rust-package.ps1` stages and validates a Rust package directory

Remaining high-value work:
- explicit migration/versioning policy for upgrades
- release checklist and distribution flow beyond the local package validator
- broader end-to-end validation on packaged builds

## Current Handoff State

If work stops mid-session, the rewrite should be resumed from these assumptions:

- The Rust app is operational enough to enqueue and run real downloads through the external backend.
- The adapter seam is established; the biggest remaining risk is service correctness under restart/interruption, not basic downloader invocation.
- The GUI is usable but still below full replacement quality.
- Packaging validation exists locally, but release engineering is still incomplete.

## Ordered Remaining Stages

Resume in this order:

1. Finish `M2` restart/interruption hardening.
   - make persisted state and recovery behavior truthful under abrupt external process termination
   - add tests that exercise those transitions explicitly
2. Expand real end-to-end validation.
   - use the packaged CLI / package validator path to confirm real adapter behavior, not only scripted service tests
3. Polish `M3` UX gaps.
   - tighten settings validation feedback and improve retry/cancel/detail flows
4. Finish `M4` release engineering.
   - document migrations, release steps, and target build outputs

## Next Single Change

The next recommended single code change is:

- add a restart/interruption-focused service test that proves a persisted active external run is recovered into a truthful paused/recoverable state on reopen

That is the best next move because it closes the biggest remaining gap between “works” and “safe to replace the old app”.

## Immediate Next Steps

In order:

1. Add restart/interruption-focused service coverage for persisted active runs.
2. Validate those recovery expectations against the real external adapter path where practical.
3. Tighten the remaining GUI UX gaps around settings/errors/details.
4. Expand release and migration documentation around the package validator path.

## Step 3: Persistence Model and SQLite Schema

The current scaffold persists queue/history/logs by serializing large protocol objects.
That is acceptable for a throwaway milestone, but not for a long-lived desktop app.

The real persistence layer should use SQLite as a normalized source of truth, with only
small JSON payloads where the shape is intentionally flexible.

### Persistence principles

- Queue, history, items, and logs should be queryable without deserializing a full app snapshot.
- Runtime-only state must not be persisted if it can become stale or misleading after restart.
- Persisted state must be restart-safe across crashes and power loss.
- The storage layer should support schema migrations from day one.
- Keep frequently-filtered fields in columns, not buried in JSON blobs.

### What should be persisted

- App settings
- Queue jobs
- Queue items
- Job options used for the run
- History entries
- Output records for completed items
- Integrity/duplicate decisions
- Logs
- Lightweight migration metadata

### What should NOT be persisted as live runtime state

- In-memory backend handles
- Running downloader task references
- Open pipe/socket ids
- Timer handles
- Temporary instantaneous backoff countdown values
- “currently running” state that cannot survive restart truthfully

On restart:
- any persisted `running` job/item should be recovered into a paused/recoverable state
- active runtime handles must always be rebuilt from scratch

### Recommended tables

#### `schema_migrations`

Tracks schema versioning.

Columns:
- `version INTEGER PRIMARY KEY`
- `applied_at_ms INTEGER NOT NULL`

#### `app_settings`

Stores durable settings as key/value rows first; can be expanded later if needed.

Columns:
- `key TEXT PRIMARY KEY`
- `value_json TEXT NOT NULL`
- `updated_at_ms INTEGER NOT NULL`

Examples:
- destination
- format
- concurrency
- organizer settings
- sentry settings
- scheduler settings
- remote/web settings

#### `queue_jobs`

Stores queued and recoverable jobs.

Columns:
- `job_id TEXT PRIMARY KEY`
- `position INTEGER NOT NULL`
- `label TEXT NOT NULL`
- `source_kind TEXT NOT NULL`
- `source_summary TEXT NOT NULL`
- `state TEXT NOT NULL`
- `created_at_ms INTEGER NOT NULL`
- `updated_at_ms INTEGER NOT NULL`
- `started_at_ms INTEGER`
- `finished_at_ms INTEGER`
- `error_code TEXT`
- `error_message TEXT`
- `options_json TEXT NOT NULL`
- `totals_json TEXT NOT NULL`

Notes:
- `state` should never remain `running` after a clean load/recovery pass
- `options_json` is acceptable because job options are cohesive and versionable
- `totals_json` can store counters until they justify dedicated columns

#### `queue_items`

Stores item-level queue state.

Columns:
- `item_id TEXT PRIMARY KEY`
- `job_id TEXT NOT NULL REFERENCES queue_jobs(job_id) ON DELETE CASCADE`
- `position INTEGER NOT NULL`
- `url TEXT NOT NULL`
- `label TEXT NOT NULL`
- `state TEXT NOT NULL`
- `created_at_ms INTEGER NOT NULL`
- `updated_at_ms INTEGER NOT NULL`
- `started_at_ms INTEGER`
- `finished_at_ms INTEGER`
- `progress_current INTEGER NOT NULL`
- `progress_total INTEGER NOT NULL`
- `progress_percent INTEGER NOT NULL`
- `progress_detail TEXT NOT NULL`
- `failure_reason_json TEXT`
- `metadata_json TEXT NOT NULL`

#### `history_jobs`

Stores completed/cancelled jobs as immutable history summaries.

Columns:
- `history_id INTEGER PRIMARY KEY AUTOINCREMENT`
- `job_id TEXT NOT NULL UNIQUE`
- `label TEXT NOT NULL`
- `source_kind TEXT NOT NULL`
- `source_summary TEXT NOT NULL`
- `final_state TEXT NOT NULL`
- `created_at_ms INTEGER NOT NULL`
- `started_at_ms INTEGER`
- `finished_at_ms INTEGER NOT NULL`
- `options_json TEXT NOT NULL`
- `totals_json TEXT NOT NULL`

#### `history_items`

Stores item-level history for completed jobs.

Columns:
- `history_item_id INTEGER PRIMARY KEY AUTOINCREMENT`
- `job_id TEXT NOT NULL REFERENCES history_jobs(job_id) ON DELETE CASCADE`
- `item_id TEXT NOT NULL`
- `position INTEGER NOT NULL`
- `url TEXT NOT NULL`
- `label TEXT NOT NULL`
- `final_state TEXT NOT NULL`
- `started_at_ms INTEGER`
- `finished_at_ms INTEGER`
- `failure_reason_json TEXT`
- `metadata_json TEXT NOT NULL`

#### `item_outputs`

Stores explicit downloader/organizer results. This replaces GUI-side filesystem inference.

Columns:
- `output_id INTEGER PRIMARY KEY AUTOINCREMENT`
- `job_id TEXT NOT NULL`
- `item_id TEXT NOT NULL`
- `kind TEXT NOT NULL`
- `final_path TEXT NOT NULL`
- `artist TEXT NOT NULL`
- `album TEXT NOT NULL`
- `title TEXT NOT NULL`
- `size_bytes INTEGER NOT NULL`
- `format TEXT NOT NULL`
- `details_json TEXT NOT NULL`

Examples of `kind`:
- `written`
- `replaced`
- `kept_both`
- `skipped_existing`

#### `item_flags`

Stores structured integrity and duplicate decisions.

Columns:
- `flag_id INTEGER PRIMARY KEY AUTOINCREMENT`
- `job_id TEXT NOT NULL`
- `item_id TEXT NOT NULL`
- `flag_type TEXT NOT NULL`
- `severity TEXT NOT NULL`
- `message TEXT NOT NULL`
- `details_json TEXT NOT NULL`
- `created_at_ms INTEGER NOT NULL`

Examples of `flag_type`:
- `integrity`
- `duplicate_decision`
- `rate_limit`
- `organizer_warning`

#### `app_logs`

Stores append-only logs for diagnostics and UI display.

Columns:
- `seq INTEGER PRIMARY KEY`
- `timestamp_ms INTEGER NOT NULL`
- `level TEXT NOT NULL`
- `scope TEXT NOT NULL`
- `job_id TEXT`
- `item_id TEXT`
- `message TEXT NOT NULL`
- `details_json TEXT NOT NULL`

### Recommended indexes

- `queue_jobs(position)`
- `queue_jobs(state)`
- `queue_items(job_id, position)`
- `queue_items(job_id, state)`
- `history_jobs(finished_at_ms DESC)`
- `history_items(job_id, position)`
- `item_outputs(job_id, item_id)`
- `item_flags(job_id, item_id, flag_type)`
- `app_logs(timestamp_ms DESC)`

### Suggested recovery rules

On service startup:

- convert persisted `queue_jobs.state = running` to `paused`
- convert persisted `queue_items.state = running` to `paused`
- clear any stale active job pointer
- preserve progress counters and timestamps for inspection
- append a recovery log entry explaining the state transition

### Immediate storage refactor target

The next storage implementation should:

1. Keep `app_settings` as key/value JSON rows.
2. Split queue jobs/items into dedicated tables.
3. Split history jobs/items into dedicated tables.
4. Store outputs and flags explicitly.
5. Keep logs append-only and queryable.
6. Stop treating the full `AppSnapshot` as the only persisted unit.
