# Personal Orchestration System Specification

## Goal

Build a local-first personal orchestration system for managing work across humans and AI agents.

The system provides a Kanban-style TUI as one view over declarative, filesystem-backed resources.

## Core Principles

* Text files are the source of truth.
* No database.
* Git is external.
* Jira reconciliation is out of scope for v1.
* Tasks use locally generated IDs.
* Board state is separate from task content.
* Workers receive only the selected task and explicit context.
* Worker/controller communication must be deterministic.
* Do not rely on LLMs following prose instructions for state transitions.
* The resource model is Kubernetes-inspired.
* **The controller is the single writer/owner of all authoritative state.** Human
  intents and worker events are both just inputs to the controller.
* **The TUI never mutates state directly.** It sends intents to the controller and
  renders observed state; the model is the source of truth and the TUI is a projection.
* **The controller is stateless.** All authoritative state lives in `.kanban/` files.
  The controller (and the TUI) can be killed and restarted at any point without losing
  or corrupting state — on startup it re-reads `.kanban/` and resumes.

## Architecture

The system has two architecturally separated components behind a defined interface:

* **Controller** — a long-running daemon. The single owner and writer of all
  authoritative state (`board.yaml`, `tasks/`, `sessions/`). It services intents from
  clients, ingests worker events from the hook intake spool, and reconciles state. It
  holds no authoritative state in memory; killing and restarting it simply re-reads the
  filesystem.
* **TUI** — a thin client. Sends intents to the controller (e.g. over a unix socket),
  observes the resulting state (by watching the files or subscribing to the controller),
  and redraws. It performs no writes to authoritative state.

The component boundary is defined by an interface, so the deployment shape (separate
daemon vs. embedded) is swappable. For v1 the controller is a long-running daemon.

Because there is exactly one writer, there is no shared-write contention: a human "move
card" and a worker phase change are the same kind of thing — inputs the controller
services and then persists.

## Storage Layout

```text id="susud2"
.kanban/
  config.yaml
  board.yaml
  hooks/
    claude-notification          # global hook script (executable), installed once
  tasks/
    task-0001/
      task.yaml
      description.md
      notes.md
    task-0002/
      task.yaml
      description.md
      notes.md
  sessions/
    task-0001/
      session.yaml
      events.yaml
      handoff.md
      transcript.jsonl           # symlink -> Claude's own transcript
      description.md             # symlink -> ../../tasks/task-0001/description.md
      notes.md                   # symlink -> ../../tasks/task-0001/notes.md
      hooks/
        intake/                  # hook scripts drop raw payloads here
        processed/               # controller moves payloads here after ingest
  archive/
    task-0003/                   # archived tasks (moved out of tasks/ and off the board)
```

## Config Resource

`config.yaml` holds global configuration: the worker adapter definitions (see
[Worker Integration](#worker-integration)) and any future global settings. See that
section for the `workers:` block.

## Board Resource

`board.yaml` is owned and written exclusively by the controller. It stores the canonical
column placement and ordering of cards. Clients change it only by sending intents.

```yaml id="yzshic"
apiVersion: kanban.local/v1alpha1
kind: Board
metadata:
  name: default
spec:
  columns:
    - id: inbox
      title: Inbox
    - id: ready
      title: Ready
    - id: doing
      title: Doing
    - id: blocked
      title: Blocked
    - id: waiting-human
      title: Waiting for Human
    - id: review
      title: Review
    - id: done
      title: Done

  cards:
    inbox:
      - task-0001
    ready: []
    doing:
      - task-0002
    blocked: []
    waiting-human: []
    review: []
    done: []
```

Card order within a column is the list order. The controller moves cards both in
response to human intents and in response to worker events (see
[State Derivation](#state-derivation)).

## Task Resource

A task's content (`task.yaml`, `description.md`, `notes.md`) is independent of its board
position. Lifecycle state is **not** stored on the task; runtime worker state lives in
the `WorkerSession` and is derived from events.

Task IDs are zero-padded sequential (`task-NNNN`), assigned by the controller on task
creation. Because the controller is the single writer, ID allocation is collision-free
without locking.

```yaml id="wnsfll"
apiVersion: kanban.local/v1alpha1
kind: Task
metadata:
  name: task-0001
  creationTimestamp: "2026-06-17T09:00:00Z"
  labels:
    area: tooling
    priority: medium
spec:
  title: Build structured task format
  summary: Define YAML-backed task resources with linked Markdown detail.
  color: blue

  descriptionRef: description.md
  notesRef: notes.md

  acceptanceCriteria:
    - Task metadata is machine-readable.
    - Long-form description is stored in Markdown.
    - Board state remains outside the task.
    - Worker handoff surfaces only allowlisted task content.

  # Optional working directory the worker starts in (cwd). Typically a repo under a
  # configured base directory. Falls back to the session workspace when unset.
  repo: ~/vcs/my-project

  jira:
    key: null
    url: null

  context:
    # `include` is the authoritative allowlist: ONLY these files are symlinked into the
    # worker session and made visible to the worker. Paths reaching outside the task
    # directory (e.g. shared docs) are permitted by design and symlinked under flattened
    # names. `exclude` is an optional guard that removes paths even if an include glob
    # would otherwise match them; on conflict, exclude wins.
    include:
      - description.md
      - notes.md
      - ../../docs/storage.md
    exclude:
      - ../../secrets/

status:
  # controller-owned
  workerSessionRef: null          # resolved by name within the .kanban namespace
  updatedAt: null
```

## Description File

`description.md`

```markdown id="yn1mrw"
# Build structured task format

Define a local task format that separates structured task metadata from prose-heavy task detail.

The TUI should read `task.yaml` for rendering and actions, but should open `description.md` for full detail.
```

## Notes File

`notes.md`

```markdown id="w12734"
# Notes

- Keep board state outside the task.
- Prefer stable local IDs.
- Make worker context explicit.
```

## TUI Requirements

The TUI is a view over controller-owned state. All mutating actions are sent to the
controller as intents (see [Intents](#intents)); the TUI never writes authoritative
files itself.

The TUI must support:

* arbitrary columns from `board.yaml`
* card colours
* vim-like shortcuts
* creating tasks
* editing tasks
* moving cards between columns
* reordering cards
* opening task detail
* searching/filtering
* handing off tasks to workers
* attaching to active worker sessions
* opening linked Jira tickets, if present

Suggested shortcuts:

```text id="lz2ihz"
h / l   move between columns
j / k   move between cards
enter   open selected card
q       quit / go back
a       add card
e       edit card
d       archive card
/       search
?       help
H / L   move card left/right
J / K   reorder card
c       hand off to worker
t       attach to worker terminal/session
o       open task actions
```

Archiving a card (`d`) is a soft delete: the controller moves the task directory to
`.kanban/archive/<task-id>/` and removes it from the board. Archived tasks are not
shown on the board.

## Intents

Clients (the TUI) mutate state only by sending intents to the controller. The controller
validates each intent, performs the corresponding write, and the change becomes visible
when the client re-reads state. Core intents:

* `create-task`
* `edit-task`
* `move-card` (column and/or position)
* `reorder-card`
* `archive-task`
* `handoff` (start a worker session for a task)
* `attach` (request reattach info for a worker session)

## State Derivation

`status.phase` is **not** stored on the task. The worker's runtime state lives in the
`WorkerSession` and is derived by the controller from the session's event stream — the
latest event determines the phase. The controller derives state, then reconciles the
card's column.

| Latest worker event                       | Derived phase   | needsHumanInput | Board effect          |
|-------------------------------------------|-----------------|-----------------|-----------------------|
| handoff issued                            | working         | false           | move card to `doing`  |
| `UserPromptSubmit`                        | working         | false           | (stay)                |
| `PreToolUse` / `PostToolUse` / `Stop`     | working         | false           | (stay)                |
| `Notification` (`notification_type=permission_prompt`) | waiting-human | true | move card to `blocked` |
| `Notification` (`notification_type=idle_prompt`)       | idle          | true | move card to `waiting-human` |
| `SessionEnd` (clean) / non-zero exit          | completed       | false           | move card to `review` |
| `StopFailure` / abnormal termination          | failed          | false           | move card to `review` |

`needsHumanInput` is therefore a derived view, not a stored flag: it is `true` whenever
the latest event is a `Notification` of type `permission_prompt` or `idle_prompt`, and
returns to `false` as soon as a `UserPromptSubmit` event arrives. This makes the
human-input flow fully deterministic (the human responding produces a real event), with
no fragile flag-clearing.

A human move intent that conflicts with worker-driven placement is simply the latest
input the controller services; the controller persists it. A subsequent worker event may
move the card again.

## Worker Handoff

A handoff creates a per-task workspace and launches a configured worker command.

Generated workspace:

```text id="tf4auy"
.kanban/sessions/task-0001/
  session.yaml
  events.yaml
  handoff.md
  transcript.jsonl               # symlink to Claude's transcript
  description.md                 # symlink to ../../tasks/task-0001/description.md
  notes.md                       # symlink to ../../tasks/task-0001/notes.md
  hooks/
    intake/
    processed/
```

The worker is launched with `--add-dir` for the session directory plus each configured
base directory (see [Agent Access and Working Directory](#agent-access-and-working-directory)),
and with its cwd set to the task's `repo` (or the session workspace if unset). Each file in
the task's `context.include` allowlist is symlinked into the session directory, so task
content is scoped exactly by which symlinks exist. Writes to `notes.md` follow the symlink
to the canonical file, so worker findings persist on the task. External references (e.g.
`../../docs/storage.md`) are symlinked under flattened names so they do not escape the
session directory.

Note: symlink behaviour can vary across tools and platforms; treat robust symlink
handling as an implementation concern of the handoff step.

Example `handoff.md`:

```markdown id="jzsdhi"
# Task handoff: task-0001

## Title

Build structured task format

## Summary

Define YAML-backed task resources with linked Markdown detail.

## Description

See `description.md`.

## Acceptance criteria

- Task metadata is machine-readable.
- Long-form description is stored in Markdown.
- Board state remains outside the task.
- Worker handoff surfaces only allowlisted task content.

## Allowed context

- description.md
- notes.md
- ../../docs/storage.md

## Instructions

Work only on this task unless explicitly asked otherwise.
Do not inspect unrelated task directories.
Update `notes.md` with useful findings.
```

The handoff prompt may guide the worker, but system state must not depend on the worker
voluntarily following prose instructions.

## Worker Integration

Use a pluggable worker adapter model.

For v1, the primary adapter should be a shell-command adapter with tmux support.

Example config (in `config.yaml`):

```yaml id="f6qoxk"
agents:
  # Directories every worker may access, in addition to its session workspace.
  # Entries may be globs and are expanded at handoff time. This is an intentional,
  # global grant — workers can read/write all matched directories regardless of task.
  baseDirs:
    - ~/vcs/*

workers:
  claude:
    command: claude
    args:
      - --add-dir
      - .kanban/sessions/{task_id}
    # cwd the worker starts in; resolves from task.spec.repo, defaulting to the
    # session workspace when the task sets no repo.
    workdir: "{repo}"
    terminal:
      type: tmux
      sessionName: kanban-{task_id}
```

At handoff the controller appends an `--add-dir <dir>` for each resolved
`agents.baseDirs` entry to the launch command, and starts the worker with its cwd set to
`workdir`. The orchestration system stores enough information to reattach to the worker
session.

## Agent Access and Working Directory

Worker access has two independent layers:

* **Session sandbox (task content).** The symlink allowlist (`context.include`) governs
  which task files are surfaced into the session directory. This is the per-task,
  explicitly-scoped layer.
* **Base directories (standing grant).** Every worker additionally receives access to the
  directories configured in `agents.baseDirs` (e.g. `~/vcs/*`), independent of the task.
  This is a deliberate, global grant so agents can reach your code repositories. Entries
  may be globs and are expanded at handoff (`~/vcs/*` grants each repo under `~/vcs`).

These are orthogonal: the allowlist scopes *task content*; base directories grant *code
access*. The base-directory grant intentionally relaxes strict per-task isolation — it is
a trust decision appropriate to a single-user local tool, not an accident.

**Working directory.** A task may set `spec.repo` to choose which directory the worker
*starts in* (its cwd), even though it can reach all base directories. When `spec.repo` is
unset, the worker starts in its session workspace. For the tmux adapter this maps to the
session's start-directory (e.g. `tmux new-session -c <workdir>`).

## Worker Session Resource

`status.phase` is derived by the controller from the event stream (see
[State Derivation](#state-derivation)); it is recorded here for convenience but is always
a function of the events.

```yaml id="v5frxl"
apiVersion: kanban.local/v1alpha1
kind: WorkerSession
metadata:
  name: task-0001-claude
  labels:
    worker: claude
spec:
  taskRef:
    name: task-0001            # resolved by name within the .kanban namespace
  worker: claude
  workspace: .kanban/sessions/task-0001
  workdir: ~/vcs/my-project        # resolved cwd; session workspace if task.spec.repo unset
  terminal:
    type: tmux
    sessionName: kanban-task-0001
  command:
    - claude
    - --add-dir
    - .kanban/sessions/task-0001
    - --add-dir                    # one per resolved agents.baseDirs entry
    - ~/vcs/repo-a
    - --add-dir
    - ~/vcs/repo-b
status:
  # all controller-owned, derived from events
  phase: working               # working | waiting-human | idle | completed | failed
  needsHumanInput: false        # derived: true iff latest event is permission_prompt/idle_prompt
  startedAt: null
  completedAt: null
  lastEventRef: null
  transcriptRef: transcript.jsonl
```

## Deterministic Worker/Controller Communication

Worker/controller communication must be based on deterministic integration points, not
natural-language inference.

Valid communication mechanisms include:

* Claude Code hooks
* process exit codes
* wrapper scripts
* explicit event files written by trusted adapters
* structured logs emitted by a wrapper
* MCP/tool calls, if added later

Invalid mechanisms for core state transitions:

* scraping terminal text
* guessing from LLM output
* asking the model to remember to update status
* relying on prose instructions for lifecycle events

## Claude Code Hook Integration

The Claude adapter uses Claude Code hooks. A single global hook script,
`.kanban/hooks/claude-notification`, is installed once and referenced by every session's
hook config. The script does **not** write authoritative state (it cannot — the
controller owns `events.yaml`). Instead it writes a raw, write-once payload into the
session's intake spool, which the controller drains.

The script learns its `task_id` from the session working directory / the payload's
`session_id`, and writes to `sessions/<task_id>/hooks/intake/<event-id>.json`.

The system subscribes to these hooks:

| Hook                         | Purpose                                                        |
|------------------------------|----------------------------------------------------------------|
| `SessionStart`               | mark session started                                           |
| `UserPromptSubmit`           | human responded — clears `needsHumanInput`                     |
| `Stop`                       | response finished                                              |
| `Notification`               | carries `notification_type` (`permission_prompt`, `idle_prompt`, …) — raises `needsHumanInput` |
| `SessionEnd`                 | session terminated — drives completion                         |

The `Notification` payload includes a `notification_type` field distinguishing
`permission_prompt` (Claude is blocked needing permission) from `idle_prompt` (Claude
finished and awaits the next prompt), plus a `transcript_path` the controller uses to
locate Claude's transcript.

Example hook config:

```json id="kboxyv"
{
  "hooks": {
    "Notification": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": ".kanban/hooks/claude-notification"
          }
        ]
      }
    ]
  }
}
```

Example behavior:

```text id="j51gue"
Claude Code Notification hook fires
  ↓
hook script writes payload to sessions/<task_id>/hooks/intake/<event-id>.json
  ↓
controller drains intake, appends WorkerEvent to events.yaml, moves payload to processed/
  ↓
controller derives needsHumanInput = true from the latest event
  ↓
TUI marks task as waiting for human
  ↓
user attaches to tmux session and replies
  ↓
UserPromptSubmit hook fires → needsHumanInput derived back to false
```

## Event Stream

Each worker session maintains a structured, append-only event stream in `events.yaml`,
written exclusively by the controller. A `WorkerEvent` is the schema of a single item in
that list (not a separate file). The raw hook payload for an event is retained under the
session for audit, and `payloadRef` points to its processed location.

Once-only processing and restart safety are provided by the intake spool itself: the
controller ingests each payload from `hooks/intake/` exactly once, records the event, and
then moves the file to `hooks/processed/`. The atomic move is the once-only guarantee — no
`lastAppliedEventRef` bookkeeping is required. On restart, the controller processes
whatever remains in `intake/` and leaves `processed/` untouched.

Example `events.yaml`:

```yaml id="u5h4h6"
apiVersion: kanban.local/v1alpha1
kind: WorkerEventList
metadata:
  name: task-0001-events
items:
  - type: started
    source: controller
    observedAt: "2026-06-17T10:00:00Z"

  - type: human_input_required
    source: claude-code-hook
    notificationType: permission_prompt
    observedAt: "2026-06-17T10:30:00Z"
    payloadRef: hooks/processed/event-0002.json
```

The TUI renders relevant event-derived state, such as:

* working
* waiting for human
* completed
* failed

## Human Input Flow

When the controller derives `needsHumanInput: true`, the TUI offers an action to attach
to the worker session. For v1, human input is given directly through the live
tmux/terminal session.

Flow:

```text id="k7v5fh"
Claude requires input
  ↓
Claude Code Notification hook fires (permission_prompt | idle_prompt)
  ↓
controller derives needsHumanInput = true
  ↓
TUI highlights task
  ↓
User presses `t`
  ↓
TUI attaches to tmux session
  ↓
User answers Claude directly
  ↓
UserPromptSubmit hook fires → controller derives needsHumanInput = false
```

The system does not need to proxy user input in v1.

## Transcript

The transcript is owned by Claude Code, not produced by this system. Every hook payload
includes a `transcript_path` field pointing at Claude's own transcript (JSONL). On
handoff/first event the controller symlinks that file into the session directory as
`transcript.jsonl`, and `transcriptRef` points at the symlink.

## Jira

Jira is optional per task.

V1 only needs to:

* store Jira key/url
* show Jira indicator on cards
* open Jira URL

No Jira reconciliation in v1.

## Safety

The system should:

* avoid showing workers the full board by default
* scope task content via explicit context allowlists (only allowlisted files are symlinked into the session)
* treat `agents.baseDirs` (e.g. `~/vcs/*`) as a deliberate, broad grant — it is the one
  place strict per-task isolation is intentionally relaxed, so keep it narrow and explicit
* warn before overwriting files changed externally
* avoid writing unrelated task files
* show active worker session indicators
* show waiting-for-human indicators based on deterministic hook events
* avoid interpreting free-form LLM output as authoritative state

## Out of Scope for v1

* Jira sync/reconciliation
* database backend
* Kubernetes backend
* Git automation
* hosted web UI
* multi-user collaboration
* full MCP server
* automatic global context discovery
* natural-language terminal scraping
* proxying human input into Claude sessions
* Agent SDK-based worker integration (richer state signals, but a different execution model)
