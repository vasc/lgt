# lgt

An observability-minded `tee` replacement. Everything piped through `lgt` is
passed straight back out — and kept, so you can browse it, live-tail it, and
correlate it with the other stages of the same pipeline, while the pipeline is
still running.

```
make build 2>&1 | lgt build          # capture under a name, output unchanged
cmd1 | lgt step1 | cmd2 | lgt step2  # multiple instances, auto-linked
lgt                                  # open the TUI, watch it all live
```

## Why

`tee` gives you a file and nothing else. When a long pipeline is running you
end up with a pile of `/tmp/out1.log` files, no idea which belongs to which
run, and `tail -f` juggling. `lgt` owns its logs: every capture lands in
lgt's own data directory with metadata (name, pipeline, timestamps, working
directory, status), and one TUI watches any of them — including captures
still being written. Your project directories stay clean unless you
explicitly ask for a local copy.

## Usage

### Capture (the default when stdin is a pipe)

```
cmd | lgt [NAME] [-g GROUP] [-o FILE] [--drain]
```

- Piped input is echoed to stdout unchanged (binary-safe, flushed per chunk).
- `NAME` is an identifier you can reference later: `lgt cat NAME`,
  `lgt rm NAME`, or in the TUI. Names may repeat across runs — every run is
  kept as its own stream; a bare name resolves to the most recent one, and
  exact stream ids always win.
- `-o FILE` additionally writes a copy to `FILE`, like classic `tee`. The
  primary log always lives in lgt's data directory; the local copy is opt-in.
- `--drain`: by default `lgt` behaves like `tee` when the downstream side of
  the pipe goes away — it stops, and exits with status 141 exactly as a
  SIGPIPE-killed `tee` would, so `set -o pipefail` pipelines behave
  identically. With `--drain` it instead keeps capturing to the log until
  upstream EOF and exits 0 (careful with infinite producers: draining keeps
  the upstream alive).

### Browse and watch

```
lgt              # TUI (when stdin is a terminal)
lgt tui          # same, explicit
lgt ls           # table of captured streams
lgt cat NAME     # dump a capture
lgt cat -f NAME  # follow a capture until it finishes (like tail -f)
```

TUI keys — global: `q` quit · `tab` switch pane · `f` toggle follow ·
`/` search (typing in the content pane; `enter` commits, `esc` cancels) ·
`d` delete · `a` archive (restore, when browsing the archive). `d`/`a` act
on the selected stream, or on every finished stream of the selected group,
and always ask `y/n` first; running streams are never touched. Stream list
pane: `j/k` move · `PgUp/PgDn` jump · `enter`/`space` open a stream, or
collapse/expand a group · `g/G` first/last entry. Content pane: `j/k` scroll
(`k` drops out of follow mode) · `PgUp/PgDn` page · `g` top · `G` bottom and
re-enable follow · `n/N` next/previous search match · `←/→` pan long lines.

The TUI refreshes automatically: new captures appear as they start, running
streams grow in place (their live byte counts show in the list), and follow
mode keeps the newest lines in view.

## Managing history: archive and clean

**Full history is the default.** lgt never deletes, archives, rotates, caps,
or overwrites a capture on its own — every run is kept until you
deliberately act on it. Nothing is shadowed either: a repeated name only
affects what the bare name resolves to; older runs remain listed,
browsable, and addressable by id.

When history grows large enough to slow things down, you have two
deliberate tools:

**Archive** moves finished streams out of the way. Archived streams cost
nothing in day-to-day use — `lgt`, `lgt ls` and name lookups never scan
them — but they are kept intact and can be reached any time:

```
lgt archive 24h              # archive finished streams idle for over 24h
lgt archive --keep 500       # keep the newest 500 active, archive the rest
lgt archive 7d --dry-run     # show what would move, change nothing
lgt archive --everything     # archive every finished stream
lgt archive 30d --include-running  # …and captures silent for 30 days
lgt tui --archived           # browse the archive (same TUI)
lgt ls --archived            # list it
lgt cat --archived NAME      # read an archived stream by name
lgt cat ID                   # an exact id is found in the archive too
lgt unarchive NAME|ID...     # bring specific streams back
lgt unarchive --since 2d     # bring back everything active in the last 2 days
```

**Clean** deletes permanently, with the same selection:

```
lgt clean 30d                # delete finished streams idle for over 30 days
lgt clean --max-size 5G      # delete oldest finished streams until ≤ 5GiB
lgt clean --archived 90d     # delete from the archive only
lgt clean --everything       # delete every finished stream
lgt clean 30d --include-running   # …stuck captures silent for 30d included
lgt rm NAME|ID...            # delete specific finished streams
lgt rm -f NAME               # also allowed for a stream still running
```

Selection, shared by `archive` and `clean` — **one is required**, so a bare
`lgt clean` or `lgt archive` does nothing but explain itself:

- `AGE` (positional): streams whose *last activity* is older than AGE —
  `90m`, `24h`, `30d`, `2w`, or compound like `3d12h`. Last activity is
  when a stream finished (or, for a running one, last wrote), so a
  three-day build that finished an hour ago is not "older than 24h".
- `--keep N`: keep only the newest N streams (running ones count towards N).
- `--max-size SIZE`: oldest first until the shelf fits SIZE (`500M`, `5G`).
- `--everything`: every finished stream.
- Every given limit is independent — a stream is selected if it violates
  *any* of them — so each limit on its own is a guarantee.
- Running streams are never selected (except with `--include-running`),
  and `--dry-run` lists the selection with sizes and changes nothing.
- A failure on one stream (say, a permission problem) does not stop the
  rest; every failure is reported and the command exits non-zero.

No subcommand reads its input. A *bare* subcommand — `lgt ls`, `lgt path`,
`lgt tui`, or `clean`/`archive` given no selection — refuses to run when a
live pipe is attached to stdin (`cmd | lgt ls`, `echo out | lgt archive`):
that is almost certainly a capture whose name collides with a subcommand,
and running anyway would swallow the pipeline's output. A subcommand given
arguments is clearly deliberate and always runs, so scripts like
`… | while read id; do lgt rm "$id"; done` work. A terminal, `/dev/null`,
or the empty, already-closed pipe that cron and similar runners give their
jobs are always fine, so `lgt archive 7d` works from a crontab as is. To
run a bare subcommand over a live pipe on purpose (e.g. `$(lgt path)` in a
script that is itself piped), redirect stdin: `lgt path </dev/null`.

Removing a live capture (`rm -f`, `clean --include-running`) discards the
rest of its log but never breaks the running pipeline itself. The only
files lgt tidies by itself are expired pipeline-linking claim files in
`groups/` and orphaned `.meta-*.tmp` files left by an interrupted metadata
write — plumbing that contains no captured data.

### The TUI tells you when to archive

The TUI times its own work — the initial load and each background refresh
(reading metadata and rebuilding the list), and each redraw. When any of
them stops feeling instantaneous (load over 100ms, refresh or redraw over
50ms on average over at least 5 samples) a yellow line appears at the top.
When the cause is the amount of history, it suggests a concrete command
sized from the measurement, e.g.

```
⚠ slow: loading 5000 streams took 112ms (budget 100ms) — run `lgt archive 58d22h` to archive 1433
```

The suggested age keeps roughly as many streams as fit the budget (with 20%
headroom), counts only finished streams (running ones are never archived),
and in the archive view suggests `lgt clean --archived AGE` instead. Slow
*redraws* are reported without a suggestion: drawing only touches the rows
on screen, so its cost comes from the terminal or connection, not from
history. The warning updates by itself — including clearing once the shelf
is small enough, even if you archive from another terminal.

## Linking pipeline instances

When one command line contains several `lgt` instances —

```
cmd1 | lgt step1 | cmd2 | lgt step2
```

— they link **automatically**: every process in a shell pipeline shares a
process group, and the first `lgt` to start claims that process group in the
store (atomically, via a hard-linked claim file); its siblings join the same
group. The TUI shows the group as one unit, labeled with a best-effort
reconstruction of the pipeline (from process-group snapshots taken at start
and shortly after; very short-lived stages can still be missed), with its
streams in start order.

**Manual linking** works across separate pipelines: pass `-g NAME` to each
capture, or `export LGT_GROUP=NAME` once for a whole script. Named groups are
stable — every capture with the same group name lands together.

Caveat: in non-interactive shells without job control (plain `sh script.sh`),
*all* pipelines share the script shell's process group, so consecutive
pipelines started within 60 seconds of each other may auto-link into one
group. Use `-g`/`LGT_GROUP` in scripts when grouping matters.

## Storage

The log is lgt's responsibility: captures live in lgt's own data directory,
never in the directory where the pipeline ran (that directory is recorded as
`cwd` metadata instead, and shown in the TUI). The directory is chosen in
this order:

1. `--data-dir DIR` (global flag, valid on every subcommand)
2. `LGT_DATA_DIR`
3. the platform data dir, e.g. `~/Library/Application Support/lgt` on macOS

Each stream is a directory holding the raw bytes plus metadata:

```
streams/s-20260821-143007123-8142/
  data.log    # exactly what was piped through, unmodified
  meta.json   # name, group, pid, pgid, machine, pid namespace, timestamps,
              # status, byte/line counts, cwd, -o copy path, pipeline snapshot
archive/      # archived streams, same layout (created by the first archive)
groups/       # transient pipeline-group claim files
```

`data.log` is plain — `grep`/`less` work on it directly. `meta.json` is
written atomically (temp file + rename), so concurrent readers such as the
TUI never see a torn file, and is useful for external tooling (`jq`).
Archiving and restoring are a single directory rename each.

A stream whose capture process died before finalizing (kill -9, terminal
closed) still says `running` in its `meta.json`. Every command sees through
that and shows it as `died` with its real size and line count. `lgt ls` and
the TUI (in the background) also *record* the death the first time they see
it: they count the stream's lines once and write `died` to `meta.json`, so
later readers no longer probe its pid or re-scan its data. That rewrite only
finalizes metadata; the captured data is never touched. `cat` and every
`--dry-run` are strictly read-only.

A capture is only declared dead when that is certain. Each `meta.json`
records a stable id of the machine it ran on (the hardware UUID on macOS,
`/etc/machine-id` on Linux — not the hostname, which changes with the
network) and, on Linux, the PID namespace it ran in. A capture recorded
under another machine id or another PID namespace is never judged by its
pid, so a store shared between machines (an NFS home) or between
containers (a bind mount — even when the containers share or lack a
machine id) never declares someone else's capture dead. A pid that exists
but belongs to another user counts as alive.

## Design notes

- **Passthrough first.** The capture loop reads a chunk, appends it to the
  log (and the `-o` copy if any), writes it to stdout, and flushes. No line
  buffering, no decoding — binary data survives byte-for-byte, and
  downstream sees data with minimal latency.
- **Broken pipe semantics match `tee`,** including the exit status (141), so
  `lgt` never changes how a pipeline terminates (`cmd | lgt | head` still
  stops early, and `pipefail` sees the same result either way). `--drain`
  opts into keep-capturing.
- **Watching is just reading.** The capturing process only ever appends to
  `data.log`; the TUI and `cat -f` tail the file by offset (with a final
  drain pass once the stream finishes, so the tail is never clipped). No
  sockets, no daemon, no coordination — multiple viewers are free.
- **Keystrokes and frames don't get slower with history.** Finished streams
  can never change, so their metadata is read once (in parallel at
  startup). Each periodic refresh (once a second) is a directory listing
  plus, per *running* stream, a pid probe and two `stat`s — the listing is
  still linear in the number of streams on the shelf, which is what the
  slowness warning watches. Only the rows on screen are drawn, all queued
  keys are handled before the next redraw, and nothing is redrawn while
  nothing changes.
- **Streams are never loaded into memory.** The content pane keeps a sparse
  line index — an anchor every 64 lines, plus one right after every line
  longer than 4KiB — built in bounded chunks, so a multi-GB stream opens
  progressively without freezing input. Drawing reads at most a screen
  width (or 4KiB) of each visible line, so a stream that is one enormous
  line (minified JSON, a `\r` progress bar) draws as fast as any other.
  Search streams the file through a fixed window, in constant memory.
- **Liveness errs towards "running".** A recycled pid (the capture died and
  another process of yours now has its pid) makes a dead capture look
  running until that process exits; the reverse — a live capture recorded
  as dead — is what the machine-id, PID-namespace and EPERM rules above
  prevent.
- **Names are reserved-word checked.** A capture cannot be called `ls`,
  `cat`, `tui`, `rm`, `clean`, `archive`, `unarchive`, `path`, or `help`, so
  the CLI stays unambiguous.

## Limitations (current)

- Startup still reads every active stream's `meta.json` once; at tens of
  thousands of active streams that takes noticeable time (the TUI will tell
  you — archive old history to fix it). `lgt ls`, `clean`, `archive`, and
  looking a stream up by *name* (`cat`/`rm`/`unarchive NAME`) read all
  metadata of the shelf every time; an exact id is a direct lookup.
- Search (`/`) first finishes indexing the selected stream and then scans
  it synchronously; on a multi-GB stream that takes a moment.
- A name restores, removes or prints only its newest run; use ids, or
  `unarchive --since`, to act on older runs.
- Versions of lgt older than the archive feature do not understand the
  `died` status and skip such streams (they are not damaged). Captures
  recorded before the machine id existed are treated as local.
- A capture recorded in a container instance that no longer exists (its PID
  namespace is gone after a restart) can never be judged dead from here, so
  it keeps looking `running`, and `lgt cat -f` on it waits until you stop
  it. Archive or delete such streams deliberately: `lgt archive 30d
  --include-running` moves running-looking streams that have written
  nothing for 30 days.
- Containers sharing a store must see each other's `/proc` namespaces as
  different (the default); a container that shares the host's PID namespace
  *and* machine id is, correctly, treated as the same machine.
- Only stdout flows through `lgt`; capture stderr the usual way
  (`cmd 2>&1 | lgt`).
- If the upstream command block-buffers when piped, its output reaches `lgt`
  (and everything downstream) in bursts — `stdbuf -oL cmd | lgt` fixes the
  producer.
- Auto-linking depends on process groups (see the scripting caveat above),
  and pipeline labels are reconstructions, not guarantees.

## Building

```
cargo build --release   # binary at target/release/lgt
cargo install --path .  # or install as `lgt`
cargo test              # unit + integration tests
```
