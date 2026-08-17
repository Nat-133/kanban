# kanban

A local-first personal orchestration system for work split between you and AI agents.
It presents a Kanban board in the terminal, but the board is only a view: the real state
is a directory of YAML and Markdown files, and cards move because agents emit events, not
because anyone remembered to drag them.

## Model

Three pieces, deliberately separated:

- **Controller** — a daemon that is the *single writer* of all authoritative state under
  `.kanban/`. It services intents from clients, drains worker events from a hook intake
  spool, derives state, and persists. It keeps nothing authoritative in memory, so killing
  and restarting it just re-reads the filesystem.
- **TUI** — a thin client. It sends intents (create, edit, move, archive, hand off, attach)
  over HTTP and renders observed state. It never writes authoritative files.
- **Workers** — agent sessions (Claude Code in tmux, via a pluggable adapter) launched with
  a per-task workspace and an explicit allowlist of context.

Because there is exactly one writer, a human moving a card and an agent finishing a step are
the same kind of thing: inputs the controller services in order.

## Derived state, not reported state

A task's phase is never stored and never self-reported by the agent. Claude Code hooks drop
raw payloads into `sessions/<task>/hooks/intake/`; the controller ingests each exactly once
(the atomic move to `processed/` *is* the once-only guarantee), appends to an append-only
event stream, and derives the phase from the latest event. `Notification` of type
`permission_prompt` or `idle_prompt` raises "waiting for human"; a `UserPromptSubmit` clears
it. A `SessionEnd` completes the task only when the human closed the session by hand — any
unrecognised reason is treated as an interruption, so a future Claude Code release can never
silently mark work done.

A `Stop` normally means the same thing — the turn is over, the human's move — except while a
subagent is still running, when the agent is only yielding until that reports back. Each
subagent is bracketed by `PreToolUse` and `SubagentStop` into a marker file under
`sessions/<task>/background/`, and a `Stop` with any marker outstanding stays "working". A
subagent that dies without reporting leaks a marker; the next prompt clears it, and Claude
Code's own `idle_prompt` still raises the warning meanwhile, so the worst case is a late
warning rather than a stuck card.

Agents launched detached can die without emitting anything, so a liveness probe reconciles
sessions whose terminal has vanished and marks them interrupted. Recovery is
operator-triggered: opening an interrupted session resumes it with `--resume`; opening a live
one attaches, so two agents can never land on one task.

Nothing in the lifecycle depends on an LLM following prose instructions.

## Layout

```text
.kanban/
  config.yaml            # worker adapters, base directory grants
  board.yaml             # columns and card order (controller-owned)
  hooks/claude-notification
  tasks/task-0001/       # task.yaml, description.md, notes.md
  sessions/task-0001/    # session.yaml, events.yaml, handoff.md,
                         # symlinked context, hooks/{intake,processed}
  archive/               # soft-deleted tasks
```

Task content is independent of board position, and a session sees only the files symlinked
into it by the task's `context.include` allowlist. Agents additionally get a standing grant
to the configured base directories (e.g. `~/vcs/*`) so they can reach your repos — a
deliberate trust decision for a single-user local tool, and the one place per-task isolation
is relaxed.

## Usage

```sh
cargo build --release

kanban init                # create the workspace layout and a default board
kanban daemon              # run the controller (default 127.0.0.1:7777)
kanban tui                 # attach the terminal UI to a running daemon
kanban activity            # human-involvement log: interruptions, steers, profile changes
```

`kanban hook <event> --session <task-id>` is internal — the installed Claude Code hook script
calls it to record a worker event and ring the daemon's doorbell.

The TUI is vim-keyed: `h/l` and `j/k` to move, `H/L` and `J/K` to move and reorder cards,
`enter` to open, `a`/`e`/`d` to add, edit and archive, `c` to hand off to a worker, `t` to
attach to its terminal, `/` to search, `?` for help.
