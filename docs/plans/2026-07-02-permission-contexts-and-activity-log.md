# Permission Contexts & Activity Log — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Give worker sessions a broad-but-gated permission profile (thread 1) and start an append-only log of every human involvement so we can later drive interruptions down (thread 6).

**Architecture:** A *permission context* is a named profile in `config.yaml` (allow/ask/deny tool rules + reserved MCP/egress fields). A task carries a mutable `spec.profile`; at handoff the resolved context's rules are baked into the per-session `settings.json` Claude Code already reads. Separately, an append-only *activity log* — one immutable JSON file per event under `.kanban/activity/` (survives task archival, dodges concurrent-append races) — records interruptions, steers, and profile changes at event grain. State is always derived by folding, never stored mutably.

**Tech Stack:** Rust, `serde` / `serde_yml` / `serde_json`, `time`, `clap`. Tests are plain `#[test]` with `tempfile`. Run with native `cargo test` (toolchain in `~/Projects`, not the devcontainer, per project setup).

**Scope guardrails (YAGNI):**
- **One** context (`default`), fairly broad. No `extends`, no per-context egress *enforcement*, no auto-classification, no board approval UI, no PreToolUse hook. Those are later threads.
- Thread 5 (exfil) is *considered, not built*: the `default` context `ask`-gates deliberate outbound actions (push / PR / Jira writes), and the context struct reserves an `egress` field — but **real exfil containment needs the network sandbox and is explicitly out of scope.** See "Thread 5 note" at the end.

---

## Naming decisions (read once)

- The task's permission profile field is `spec.profile` (a `String`), **not** `context` — `spec.context` already means the file allowlist (`model/mod.rs:226,238`). Do not conflate them.
- The config key is `contexts` (map name → `PermissionContext`).
- Activity events are immutable facts. We emit `escalation`/`interruption` and `profileChanged` facts now; a resolution fact (`escalation_resolved`) is deferred with the board-approval thread. Latency is *derivable later* by joining, never stored.

## Precedence assumption to VERIFY before Task 4

Claude Code permission precedence is assumed to be **deny > ask > allow** (an `ask` rule overrides a broader `allow`). The `default` context relies on this: it allows broadly and `ask`s narrowly. Before implementing Task 4, confirm against the installed Claude Code that (a) the settings key is `permissions.{allow,ask,deny}` and (b) `ask` overrides `allow`. If the installed version differs, adjust the generated JSON in Task 4 only — the model/types are unaffected.

---

## Task 1: `PermissionContext` type + `contexts` in `Config`

**Files:**
- Modify: `src/model/mod.rs` (near `Config`, ~413-427)
- Test: `src/model/mod.rs` `#[cfg(test)]`

**Step 1: Write the failing test**

```rust
#[test]
fn config_parses_contexts() {
    let yaml = "\
contexts:
  default:
    allow: [\"Bash\", \"Edit\"]
    ask: [\"Bash(git push:*)\"]
";
    let cfg: Config = serde_yml::from_str(yaml).unwrap();
    let c = cfg.contexts.get("default").unwrap();
    assert_eq!(c.allow, vec!["Bash".to_string(), "Edit".to_string()]);
    assert_eq!(c.ask, vec!["Bash(git push:*)".to_string()]);
    assert!(c.deny.is_empty());
    assert!(c.mcp.is_empty());
    assert!(c.egress.is_empty());
}
```

**Step 2: Run to verify it fails**

Run: `cargo test config_parses_contexts`
Expected: FAIL — no field `contexts` / no type `PermissionContext`.

**Step 3: Implement**

Add to `Config`:
```rust
    #[serde(default)]
    pub contexts: std::collections::BTreeMap<String, PermissionContext>,
```

Add the type:
```rust
/// A named permission profile. `allow`/`ask`/`deny` are Claude Code tool-rule
/// patterns baked into a session's settings.json at handoff. `mcp` lists MCP
/// servers the context opts into loading. `egress` is RESERVED: it documents the
/// intended network-destination allowlist but is NOT enforced yet (real exfil
/// containment needs the worker sandbox — see docs/plans thread 5).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionContext {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub mcp: Vec<String>,
    /// Reserved; not enforced. See type doc.
    #[serde(default)]
    pub egress: Vec<String>,
}
```

**Step 4: Run to verify it passes**

Run: `cargo test config_parses_contexts`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/model/mod.rs
git commit -m "feat(model): PermissionContext type and contexts config map"
```

---

## Task 2: `profile` field on `TaskSpec` + context resolver

**Files:**
- Modify: `src/model/mod.rs` (`TaskSpec` ~212-227; add resolver method to `Config`)
- Test: `src/model/mod.rs`

**Step 1: Write the failing tests**

```rust
#[test]
fn task_profile_defaults_to_none_and_parses() {
    let yaml = "\
apiVersion: kanban.local/v1alpha1
kind: Task
metadata: { name: task-0001 }
spec:
  title: T
  summary: S
  descriptionRef: description.md
  notesRef: notes.md
  profile: cluster-ops
";
    let t: Task = serde_yml::from_str(yaml).unwrap();
    assert_eq!(t.spec.profile.as_deref(), Some("cluster-ops"));
}

#[test]
fn context_for_falls_back_to_builtin_default() {
    // Empty config: unknown/None profile resolves to the built-in broad default,
    // never panics, never returns an empty allowlist.
    let cfg = Config::default();
    let c = cfg.context_for(None);
    assert!(c.allow.iter().any(|p| p.starts_with("Bash")), "builtin default must be broad");
    let c2 = cfg.context_for(Some("does-not-exist"));
    assert_eq!(c, c2, "unknown profile falls back to default");
}
```

**Step 2: Run to verify they fail**

Run: `cargo test task_profile_defaults_to_none_and_parses context_for_falls_back_to_builtin_default`
Expected: FAIL — no field `profile`, no method `context_for`.

**Step 3: Implement**

Add to `TaskSpec`:
```rust
    /// Permission profile name (key into `Config.contexts`). None ⇒ "default".
    /// NOTE: distinct from `context` above, which is the file allowlist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
```

Add to `Config` an owned resolver returning the named context, else a built-in broad default:
```rust
impl Config {
    /// Resolve a task's profile to its context. Precedence: named context in
    /// config → the config's "default" context → a built-in broad fallback.
    /// Owned (not borrowed) so the built-in fallback has somewhere to live.
    pub fn context_for(&self, profile: Option<&str>) -> PermissionContext {
        let name = profile.unwrap_or("default");
        if let Some(c) = self.contexts.get(name) {
            return c.clone();
        }
        if let Some(c) = self.contexts.get("default") {
            return c.clone();
        }
        PermissionContext::builtin_default()
    }
}

impl PermissionContext {
    /// Broad fallback used when config defines no contexts: allow the common
    /// local tools, ask on deliberately-visible outbound actions, deny nothing.
    pub fn builtin_default() -> Self {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect();
        PermissionContext {
            allow: s(&["Bash", "Edit", "Write", "Read", "Glob", "Grep", "WebFetch", "WebSearch"]),
            ask: s(&[
                "Bash(git push:*)",
                "Bash(git push --force:*)",
                "Bash(gh pr create:*)",
                "Bash(gh pr merge:*)",
                "Bash(gh pr review:*)",
            ]),
            deny: vec![],
            mcp: vec![],
            egress: vec![],
        }
    }
}
```

**Step 4: Run to verify they pass**

Run: `cargo test task_profile context_for`
Expected: PASS. Also run the existing `task_parses_and_defaults_status` to confirm no regression.

**Step 5: Commit**

```bash
git add src/model/mod.rs
git commit -m "feat(model): task spec.profile and Config::context_for resolver"
```

---

## Task 3: default `config.yaml` ships a broad `default` context

**Files:**
- Modify: `src/controller/store.rs` `default_config_yaml()` (~38-40)
- Test: `src/controller/store.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn init_writes_default_permission_context() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    init_workspace(&root).unwrap();
    let cfg = load_config(&root).unwrap();
    let c = cfg.contexts.get("default").expect("default context present");
    assert!(c.allow.iter().any(|p| p.starts_with("Bash")));
    assert!(c.ask.iter().any(|p| p.contains("git push")));
}
```

**Step 2: Run to verify it fails**

Run: `cargo test init_writes_default_permission_context`
Expected: FAIL — no `default` context in shipped config.

**Step 3: Implement**

Extend `default_config_yaml()` to append a `contexts:` block mirroring `builtin_default()`. Keep it as the same YAML string style already used. Example addition:
```
contexts:
  default:
    allow: [Bash, Edit, Write, Read, Glob, Grep, WebFetch, WebSearch]
    ask:
      - Bash(git push:*)
      - Bash(git push --force:*)
      - Bash(gh pr create:*)
      - Bash(gh pr merge:*)
      - Bash(gh pr review:*)
    # egress: RESERVED — not enforced yet (needs the worker network sandbox).
```

**Step 4: Run to verify it passes**

Run: `cargo test init_writes_default_permission_context && cargo test init_writes_loadable_config`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/controller/store.rs
git commit -m "feat(store): ship a broad default permission context in config"
```

---

## Task 4: bake context permissions into the generated `settings.json`

**Files:**
- Modify: `src/controller/handoff.rs` `prepare_session` (settings assembly ~110-137)
- Test: `src/controller/handoff.rs`

**Prereq:** confirm the precedence assumption at the top of this plan.

**Step 1: Write the failing test**

```rust
#[test]
fn prepare_session_bakes_context_permissions_into_settings() {
    use crate::controller::store;
    use crate::model::TaskId;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    store::init_workspace(&root).unwrap();
    let id = TaskId::new(1);
    let task = sample_task(id, "x"); // profile None → default context
    store::save_task(&root, &task).unwrap();
    let cfg = store::load_config(&root).unwrap();
    let session = prepare_session(&root, &task, "claude",
        cfg.workers.get("claude").unwrap(), &cfg.agents.base_dirs).unwrap();

    let settings = root.join("sessions/task-0001/hooks/settings.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    let allow = v["permissions"]["allow"].as_array().unwrap();
    assert!(allow.iter().any(|x| x == "Bash"));
    let ask = v["permissions"]["ask"].as_array().unwrap();
    assert!(ask.iter().any(|x| x.as_str().unwrap().contains("git push")));
    // hooks must still be present — permissions are additive, not a replacement
    assert!(v["hooks"]["Stop"].is_array());
}
```

**Step 2: Run to verify it fails**

Run: `cargo test prepare_session_bakes_context_permissions_into_settings`
Expected: FAIL — settings has no `permissions` key.

**Step 3: Implement**

`prepare_session` needs the resolved context. Two sub-changes:

1. Load config inside `prepare_session`, OR (cleaner, avoids a second config read) resolve in `handoff()` and pass the `PermissionContext` in. **Chosen:** add a `context: &PermissionContext` parameter to `prepare_session`; `handoff()` computes it via `cfg.context_for(task.spec.profile.as_deref())`. Update the existing `prepare_session` call in `handoff()` and every test caller in this file (they currently pass 5 args — add the resolved context; use `Config::default().context_for(None)` in tests that don't care).

2. When building `settings`, add the permissions object:
```rust
let settings = serde_json::json!({
    "permissions": {
        "allow": context.allow,
        "ask": context.ask,
        "deny": context.deny,
    },
    "hooks": { /* unchanged */ }
});
```
(Leave MCP wiring out — `default.mcp` is empty. If `context.mcp` is non-empty in future, that becomes a separate `--mcp-config`/`enabledMcpjsonServers` change; add a `// TODO(thread 1): wire context.mcp` note.)

**Step 4: Run to verify it passes**

Run: `cargo test -p kanban --lib handoff`
Expected: PASS (this test + all existing handoff tests still green after the signature change).

**Step 5: Commit**

```bash
git add src/controller/handoff.rs
git commit -m "feat(handoff): bake resolved permission context into session settings.json"
```

---

## Task 5: `ActivityEvent` type

**Files:**
- Create: `src/controller/activity.rs`
- Modify: `src/controller/mod.rs` (add `pub mod activity;`)
- Test: in `activity.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn activity_event_round_trips_json() {
    let e = ActivityEvent {
        observed_at: time::OffsetDateTime::UNIX_EPOCH,
        task: TaskId::new(1),
        profile: "default".into(),
        kind: ActivityKind::Interruption { reason: InterruptionReason::PermissionPrompt },
    };
    let j = serde_json::to_string(&e).unwrap();
    let back: ActivityEvent = serde_json::from_str(&j).unwrap();
    assert_eq!(e, back);
    assert!(j.contains("\"permissionPrompt\""));
}
```

**Step 2: Run to verify it fails**

Run: `cargo test activity_event_round_trips_json`
Expected: FAIL — module/type missing.

**Step 3: Implement**

`src/controller/activity.rs` (types only for this task):
```rust
use crate::model::TaskId;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// One immutable human-involvement fact. Append-only; never mutated. Derived
/// metrics (interruptions-per-task, approve/deny ratios) are computed by folding
/// these, never stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEvent {
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    pub task: TaskId,
    /// Active profile at emit time — denormalized on purpose so the promote-query
    /// is one pass and survives later edits to the profile timeline.
    pub profile: String,
    #[serde(flatten)]
    pub kind: ActivityKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ActivityKind {
    /// The worker blocked and needs the human (a context-switch cost event).
    Interruption { reason: InterruptionReason },
    /// The human sent the worker a message / prompt.
    Steer,
    /// The human (or a future classifier) changed the task's profile.
    ProfileChanged { from: Option<String>, to: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InterruptionReason {
    PermissionPrompt,
    Idle,
}
```
Add `pub mod activity;` to `src/controller/mod.rs`.

**Step 4: Run to verify it passes**

Run: `cargo test activity_event_round_trips_json`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/controller/activity.rs src/controller/mod.rs
git commit -m "feat(activity): ActivityEvent fact type"
```

---

## Task 6: append-only activity store (root-level spool)

**Files:**
- Modify: `src/controller/activity.rs` (add store fns)
- Test: in `activity.rs`

**Design:** one file per event under `.kanban/activity/`, named `<unix_nanos>-<pid>.json` — ordered by time, unique across the hook and daemon processes, written via `store::atomic_write`. Lives at root so it **survives task archival** (unlike `sessions/<id>/`). Reading = list dir, sort by filename, parse.

**Step 1: Write the failing test**

```rust
#[test]
fn append_then_load_returns_events_in_time_order() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    std::fs::create_dir_all(&root).unwrap();
    let mk = |secs: i64, task: u32| ActivityEvent {
        observed_at: time::OffsetDateTime::from_unix_timestamp(secs).unwrap(),
        task: TaskId::new(task),
        profile: "default".into(),
        kind: ActivityKind::Steer,
    };
    append(&root, &mk(200, 2)).unwrap();
    append(&root, &mk(100, 1)).unwrap();
    let all = load(&root).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].observed_at.unix_timestamp(), 100, "sorted ascending by time");
    assert_eq!(all[1].task, TaskId::new(2));
}

#[test]
fn load_is_empty_when_no_activity_dir() {
    let dir = tempfile::tempdir().unwrap();
    assert!(load(dir.path()).unwrap().is_empty());
}
```

**Step 2: Run to verify they fail**

Run: `cargo test append_then_load_returns_events_in_time_order load_is_empty_when_no_activity_dir`
Expected: FAIL — `append`/`load` missing.

**Step 3: Implement**

```rust
use crate::controller::store;
use std::path::{Path, PathBuf};

pub fn activity_dir(root: &Path) -> PathBuf { root.join("activity") }

/// Append one immutable fact. Filename = nanos-pid so it is time-ordered and
/// unique across the hook and daemon processes; never overwrites an existing file.
pub fn append(root: &Path, event: &ActivityEvent) -> anyhow::Result<()> {
    let nanos = event.observed_at.unix_timestamp_nanos();
    let pid = std::process::id();
    let name = format!("{nanos:020}-{pid}.json");
    let path = activity_dir(root).join(name);
    store::atomic_write(&path, &serde_json::to_string(event)?)
}

/// Load every fact, ascending by filename (i.e. by time). Missing dir ⇒ empty.
pub fn load(root: &Path) -> anyhow::Result<Vec<ActivityEvent>> {
    let dir = activity_dir(root);
    if !dir.exists() { return Ok(Vec::new()); }
    let mut names: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .filter(|n| n.to_string_lossy().ends_with(".json"))
        .collect();
    names.sort();
    let mut out = Vec::new();
    for n in names {
        let text = std::fs::read_to_string(dir.join(&n))?;
        out.push(serde_json::from_str(&text)?);
    }
    Ok(out)
}
```
(Note: `atomic_write` already `create_dir_all`s the parent, so no explicit mkdir needed. The nanos are zero-padded to 20 digits so lexical sort == time sort.)

**Step 4: Run to verify they pass**

Run: `cargo test -p kanban --lib activity`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/controller/activity.rs
git commit -m "feat(activity): append-only root-level activity spool"
```

---

## Task 7: emit activity facts from the hook path

**Files:**
- Modify: `src/controller/events.rs` `record_state` (~48-59)
- Test: `src/controller/events.rs`

**Design:** when a hook firing represents human involvement, also append an `ActivityEvent`. Map from the same `IntakePayload` used for state:
- `notification` + `permission_prompt` → `Interruption{PermissionPrompt}`
- `notification` + `idle_prompt`, or `stop` → `Interruption{Idle}`
- `user-prompt-submit` → `Steer`
- everything else → no activity fact

The task's current profile is read from `task.yaml` (fallback `"default"` if the task can't be loaded). Appending activity must **not** break state recording — log-and-continue on activity errors (the state write is the critical path; the log is best-effort).

**Step 1: Write the failing test**

```rust
#[test]
fn permission_prompt_appends_interruption_activity() {
    use crate::controller::{activity, apply::apply};
    use crate::model::proto::Intent;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    store::init_workspace(&root).unwrap();
    apply(&root, Intent::CreateTask { title: "A".into(), summary: "".into(),
        column: "todo".parse().unwrap() }).unwrap();
    let id = TaskId::new(1);

    record_state(&root, id, "notification",
        "{\"notification_type\":\"permission_prompt\"}").unwrap();

    let acts = activity::load(&root).unwrap();
    assert_eq!(acts.len(), 1);
    assert_eq!(acts[0].task, id);
    assert!(matches!(acts[0].kind,
        activity::ActivityKind::Interruption {
            reason: activity::InterruptionReason::PermissionPrompt }));
}

#[test]
fn untracked_firing_appends_no_activity() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    store::init_workspace(&root).unwrap();
    record_state(&root, TaskId::new(1), "notification",
        "{\"notification_type\":\"auth_success\"}").unwrap();
    assert!(crate::controller::activity::load(&root).unwrap().is_empty());
}
```

**Step 2: Run to verify they fail**

Run: `cargo test permission_prompt_appends_interruption_activity untracked_firing_appends_no_activity`
Expected: FAIL — no activity emitted.

**Step 3: Implement**

Add a mapper in `events.rs`:
```rust
use crate::controller::activity::{self, ActivityEvent, ActivityKind, InterruptionReason};

fn to_activity(item: &IntakePayload) -> Option<ActivityKind> {
    match item.event.as_str() {
        "user-prompt-submit" => Some(ActivityKind::Steer),
        "stop" => Some(ActivityKind::Interruption { reason: InterruptionReason::Idle }),
        "notification" => match item.payload.get("notification_type").and_then(|v| v.as_str()) {
            Some("permission_prompt") => Some(ActivityKind::Interruption { reason: InterruptionReason::PermissionPrompt }),
            Some("idle_prompt") => Some(ActivityKind::Interruption { reason: InterruptionReason::Idle }),
            _ => None,
        },
        _ => None,
    }
}
```

In `record_state`, after building `item` and before/after the `to_event` match, emit activity best-effort:
```rust
if let Some(kind) = to_activity(&item) {
    let profile = store::load_task(root, id).ok()
        .and_then(|t| t.spec.profile)
        .unwrap_or_else(|| "default".to_string());
    let ev = ActivityEvent {
        observed_at: time::OffsetDateTime::now_utc(),
        task: id,
        profile,
        kind,
    };
    if let Err(e) = activity::append(root, &ev) {
        tracing::warn!(error = %e, "failed to append activity event");
    }
}
```

**Step 4: Run to verify they pass**

Run: `cargo test -p kanban --lib events`
Expected: PASS (new tests + all existing `events` tests green).

**Step 5: Commit**

```bash
git add src/controller/events.rs
git commit -m "feat(events): emit activity facts for interruptions and steers"
```

---

## Task 8: `SetProfile` intent (mutable profile + `profileChanged` fact)

**Files:**
- Modify: `src/model/proto.rs` (`Intent` ~10-19)
- Modify: `src/controller/apply.rs` (intent handler)
- Test: `src/controller/apply.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn set_profile_updates_task_and_logs_profile_changed() {
    use crate::controller::activity;
    use crate::model::proto::Intent;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    store::init_workspace(&root).unwrap();
    apply(&root, Intent::CreateTask { title: "A".into(), summary: "".into(),
        column: "todo".parse().unwrap() }).unwrap();
    let id = TaskId::new(1);

    apply(&root, Intent::SetProfile { task: id, profile: "cluster-ops".into() }).unwrap();

    assert_eq!(store::load_task(&root, id).unwrap().spec.profile.as_deref(), Some("cluster-ops"));
    let acts = activity::load(&root).unwrap();
    assert!(acts.iter().any(|a| matches!(&a.kind,
        activity::ActivityKind::ProfileChanged { to, .. } if to == "cluster-ops")));
}
```

**Step 2: Run to verify it fails**

Run: `cargo test set_profile_updates_task_and_logs_profile_changed`
Expected: FAIL — no `Intent::SetProfile`.

**Step 3: Implement**

Add to `Intent`:
```rust
    SetProfile { task: TaskId, profile: String },
```
Handle in `apply.rs` (match the surrounding style of `EditTask`/`ArchiveTask`): load task, capture `from = task.spec.profile.clone()`, set `task.spec.profile = Some(profile.clone())`, save, then `activity::append` a `ProfileChanged { from, to: profile }` with `observed_at = now`, `profile = to` (the new profile is the active one at emit time). Add a `proto.rs` round-trip test alongside the existing intent round-trip tests.

**Step 4: Run to verify it passes**

Run: `cargo test set_profile`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/model/proto.rs src/controller/apply.rs
git commit -m "feat(apply): SetProfile intent updates profile and logs the change"
```

---

## Task 9: `kanban activity` — eyeball + basic counts

**Files:**
- Modify: `src/main.rs` (add `Activity` subcommand)
- Modify: `src/controller/activity.rs` (add `summary` aggregation)
- Test: `src/controller/activity.rs` (aggregation only; CLI wiring is thin)

**Design:** the minimal "eyeball it" surface. `summary(events)` returns interruptions-per-task and per-`(profile, reason)` counts — the KPIs that are *invariant to how many tasks exist or when you went to lunch* (counts/ratios, not wall-clock rates). The subcommand prints the raw list and the summary.

**Step 1: Write the failing test**

```rust
#[test]
fn summary_counts_interruptions_per_task() {
    let mk = |task: u32, kind: ActivityKind| ActivityEvent {
        observed_at: time::OffsetDateTime::UNIX_EPOCH, task: TaskId::new(task),
        profile: "default".into(), kind };
    let evs = vec![
        mk(1, ActivityKind::Interruption { reason: InterruptionReason::PermissionPrompt }),
        mk(1, ActivityKind::Interruption { reason: InterruptionReason::Idle }),
        mk(1, ActivityKind::Steer),
        mk(2, ActivityKind::Interruption { reason: InterruptionReason::PermissionPrompt }),
    ];
    let s = summary(&evs);
    assert_eq!(s.interruptions_per_task[&TaskId::new(1)], 2); // steer is not an interruption
    assert_eq!(s.interruptions_per_task[&TaskId::new(2)], 1);
}
```

**Step 2: Run to verify it fails**

Run: `cargo test summary_counts_interruptions_per_task`
Expected: FAIL — no `summary`.

**Step 3: Implement**

Add a `Summary` struct + `summary(&[ActivityEvent]) -> Summary` that folds the events (count `Interruption` per task; ignore `Steer`/`ProfileChanged` for the interruption metric). Then add the `Activity` subcommand to `main.rs`:
```rust
    /// Show the human-involvement activity log and summary counts.
    Activity,
```
handler: `let evs = kanban::controller::activity::load(&cli.root)?;` then print each event (one line) and the summary.

**Step 4: Run to verify it passes**

Run: `cargo test -p kanban --lib activity && cargo build`
Expected: PASS + clean build.

**Step 5: Commit**

```bash
git add src/main.rs src/controller/activity.rs
git commit -m "feat(cli): kanban activity dumps the involvement log and counts"
```

---

## Task 10: archive a session by moving it, not deleting it (drop `work/`)

**Files:**
- Modify: `src/controller/store.rs` (add `archive_session_dir`; `remove_session_dir` stays for other callers/tests)
- Modify: `src/controller/apply.rs` (the `ArchiveTask` handler calls `archive_session_dir` instead of `remove_session_dir`)
- Test: `src/controller/store.rs`

**Design:** on archive, move `sessions/<id>/` → `.kanban/archive/sessions/<id>/` (top-level, parallel to `sessions/`, so the reconcile loop and `load_all_sessions` — which scan `sessions/` — never see it), then delete `work/` from the destination. Same filesystem ⇒ the move is an atomic `rename`. The transcript is already localized inside the session dir by Task 11's `SessionEnd` copy, so it rides along automatically; only the reproducible `work/` bulk is dropped.

**Step 1: Write the failing test**

```rust
#[test]
fn archive_moves_session_and_drops_work_dir() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    init_workspace(&root).unwrap();
    let id = TaskId::new(1);
    let sdir = session_dir(&root, id);
    std::fs::create_dir_all(sdir.join("work/checkout")).unwrap();
    std::fs::write(sdir.join("session.yaml"), "kind: WorkerSession\n").unwrap();
    std::fs::write(sdir.join("transcript.jsonl"), "{}\n").unwrap();

    archive_session_dir(&root, id).unwrap();

    assert!(!sdir.exists(), "live session dir removed");
    let adir = root.join("archive/sessions/task-0001");
    assert!(adir.join("session.yaml").exists(), "record kept");
    assert!(adir.join("transcript.jsonl").exists(), "transcript kept");
    assert!(!adir.join("work").exists(), "reproducible work/ dropped");
}

#[test]
fn archive_session_dir_is_noop_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    init_workspace(&root).unwrap();
    archive_session_dir(&root, TaskId::new(9)).unwrap(); // must not error
}
```

**Step 2: Run to verify they fail**

Run: `cargo test archive_moves_session_and_drops_work_dir archive_session_dir_is_noop_when_absent`
Expected: FAIL — `archive_session_dir` missing.

**Step 3: Implement**

```rust
pub fn archive_dir(root: &Path) -> PathBuf { root.join("archive/sessions") }

/// Archive a task's session: move its runtime dir out of `sessions/` into
/// `archive/sessions/<id>/` (keeping the record — session.yaml, state, transcript)
/// and drop the reproducible `work/` bulk. No-op if there is no session dir.
pub fn archive_session_dir(root: &Path, id: TaskId) -> anyhow::Result<()> {
    let src = session_dir(root, id);
    if !src.exists() {
        return Ok(());
    }
    let dst = archive_dir(root).join(id.to_string());
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    if dst.exists() {
        fs::remove_dir_all(&dst)?; // ids never reuse, but be idempotent
    }
    fs::rename(&src, &dst)?;
    let work = dst.join("work");
    if work.exists() {
        fs::remove_dir_all(&work)?;
    }
    Ok(())
}
```
Then in `apply.rs`, the `ArchiveTask` arm calls `store::archive_session_dir` where it currently calls `store::remove_session_dir`.

**Step 4: Run to verify they pass**

Run: `cargo test -p kanban --lib store && cargo test -p kanban --lib apply`
Expected: PASS (new tests + existing archive tests green).

**Step 5: Commit**

```bash
git add src/controller/store.rs src/controller/apply.rs
git commit -m "feat(store): archive sessions by moving them, dropping work/ bulk"
```

---

## Task 11: capture `session_id` + copy transcript on `SessionEnd`

**Files:**
- Modify: `src/model/mod.rs` (`WorkerSessionStatus` ~383-394: add `session_id`)
- Modify: `src/controller/events.rs` (capture from hook payload; copy on `session-end`)
- Test: `src/controller/events.rs`

**Design:** Claude Code puts `session_id` and `transcript_path` in every hook payload (the `raw_payload` `record_state` already receives). On `session-start`, store `session_id` on the session (seeds threads 3/4 — resume). On `session-end`, copy the `transcript_path` file into `sessions/<id>/transcript.jsonl` and set `transcript_ref` — because Claude GCs the original after `cleanupPeriodDays` (~30), so the archived copy must be a real copy, not a pointer. Best-effort: a missing/unreadable transcript logs a warning, never fails the hook.

**Step 1: Write the failing test**

```rust
#[test]
fn session_end_copies_transcript_into_session_dir() {
    use crate::controller::{apply::apply, handoff};
    use crate::model::proto::Intent;
    struct NoLaunch;
    impl handoff::Launcher for NoLaunch {
        fn launch(&self, _s: &crate::model::WorkerSession, _n: &str) -> anyhow::Result<()> { Ok(()) }
        fn kill(&self, _n: &str) {}
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    store::init_workspace(&root).unwrap();
    apply(&root, Intent::CreateTask { title: "A".into(), summary: "".into(),
        column: "todo".parse().unwrap() }).unwrap();
    let id = TaskId::new(1);
    handoff::handoff(&root, id, "claude", &NoLaunch).unwrap(); // writes session.yaml

    // a real transcript file somewhere outside the workspace
    let tpath = dir.path().join("orig-transcript.jsonl");
    std::fs::write(&tpath, "{\"m\":1}\n{\"m\":2}\n").unwrap();
    let payload = format!("{{\"session_id\":\"abc-123\",\"transcript_path\":\"{}\"}}",
        tpath.display());

    record_state(&root, id, "session-end", &payload).unwrap();

    let copied = store::session_dir(&root, id).join("transcript.jsonl");
    assert_eq!(std::fs::read_to_string(&copied).unwrap(), "{\"m\":1}\n{\"m\":2}\n");
    let s = store::load_session(&root, id).unwrap().unwrap();
    assert_eq!(s.status.transcript_ref.as_deref(), Some("transcript.jsonl"));
}

#[test]
fn session_start_records_session_id() {
    /* analogous: handoff, then record_state(session-start) with a session_id
       payload, then assert load_session().status.session_id == Some("abc-123") */
}
```

**Step 2: Run to verify they fail**

Run: `cargo test session_end_copies_transcript_into_session_dir session_start_records_session_id`
Expected: FAIL — no capture/copy behavior; no `session_id` field.

**Step 3: Implement**

1. Add to `WorkerSessionStatus`:
```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
```
2. In `events.rs`, after the existing `record_state` body (or as a helper it calls), handle the two side effects — only when a session.yaml exists (load/modify/save via `store::load_session`/`store::save_session`):
   - `session-start`: read `payload["session_id"]` → set `status.session_id`.
   - `session-end`: read `payload["transcript_path"]`; if it points at a readable file, copy it to `session_dir(root,id)/transcript.jsonl` and set `status.transcript_ref = "transcript.jsonl"`. Wrap in a best-effort block that `tracing::warn!`s on error.

**Step 4: Run to verify they pass**

Run: `cargo test -p kanban --lib events`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/model/mod.rs src/controller/events.rs
git commit -m "feat(session): capture session_id and copy transcript on session end"
```

---

## Final verification

Run the whole suite and a clean build:
```bash
cargo test
cargo build
```
Expected: all green.

Manual smoke (optional, needs a real worker): hand off a task, watch `.kanban/activity/` accrue one file per interruption/steer, and `kanban activity` count them.

---

## Thread 5 note (exfil) — deliberately deferred

This plan does **not** contain exfiltration. The `default` context `ask`-gates *deliberate, visible* outbound actions (push / PR), which is the *visibility* axis — not the *exfil* axis. A worker with broad `Bash` can still egress company code a hundred ways tool-rules can't enumerate (`curl`, `python -c`, `/dev/tcp`, an arbitrary git remote, an MCP fetch). Real containment requires running workers inside a network-egress-allowlisted sandbox (default-deny; allow only the Anthropic API + your git remote + trusted registries), per the container/devcontainer-firewall approach. The `PermissionContext.egress` field is reserved to carry that allowlist when the sandbox lands. **Until then, treat these permissions as convenience/visibility gating, not a security boundary.**

## Explicitly out of scope (later threads)

- PreToolUse hook + board approval UI (thread 2) → enables `escalation_resolved` facts and per-`(context,tool)` approve/deny ratios.
- Using the captured `session_id` to `--resume` relaunch (thread 3) and the tmux→SDK substrate move (thread 4). Task 11 only *captures* the id; nothing consumes it yet.
- Per-context egress enforcement + worker sandbox (thread 5).
- Auto-suggesting rule promotions from the log, and model-classified initial profile (thread 6, later half).
