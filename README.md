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
`/` search (typing in the content pane; `enter` commits, `esc` cancels).
Stream list pane: `j/k` move · `enter`/`space` open a stream, or
collapse/expand a group · `g/G` first/last entry. Content pane: `j/k` scroll
(`k` drops out of follow mode) · `PgUp/PgDn` page · `g` top · `G` bottom and
re-enable follow · `n/N` next/previous search match · `←/→` pan long lines.

The TUI refreshes automatically: new captures appear as they start, running
streams grow in place (their live byte counts show in the list), and follow
mode keeps the newest lines in view.

### Housekeeping — always explicit, never automatic

```
lgt rm NAME|ID...   # remove specific finished streams
lgt rm -f NAME      # required to remove a stream that is still running
lgt clean           # remove all finished streams
lgt clean --all     # remove everything, running captures included
lgt path            # print the data directory
```

**Full history is the default.** lgt never deletes, rotates, caps, or
overwrites a capture on its own — every run is kept forever until you
deliberately remove it with `rm`/`clean`. Nothing is shadowed either: a
repeated name only affects what the bare name resolves to; older runs remain
listed, browsable, and addressable by id. `rm` refuses streams that are
still running unless you pass `-f`; removing a live capture discards the
rest of its log but never breaks the running pipeline itself. (The only
files lgt tidies by itself are expired pipeline-linking claim files in
`groups/` — transient plumbing that contains no captured data.)

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
  meta.json   # name, group, pid, pgid, timestamps, status, byte/line counts,
              # cwd it ran from, -o copy path, pipeline snapshot
groups/       # transient pipeline-group claim files
```

`data.log` is plain — `grep`/`less` work on it directly. `meta.json` is
written atomically (temp file + rename), so concurrent readers such as the
TUI never see a torn file, and is useful for external tooling (`jq`). A
stream whose process died before finalizing (kill -9, terminal closed) shows
as `died`, and its byte/line counts are measured from the data file itself,
so they stay truthful even without a clean shutdown.

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
- **Liveness is best-effort.** "running" vs "died" is derived by probing the
  recorded pid; a recycled pid can briefly misreport a dead capture as
  running. Counts shown for non-finalized streams always come from the data
  file, so they are correct regardless.
- **Names are reserved-word checked.** A capture cannot be called `ls`,
  `cat`, `tui`, `rm`, `clean`, `path`, or `help`, so the CLI stays
  unambiguous.

## Limitations (current)

- The TUI loads a selected stream fully into memory; multi-GB captures will
  be slow to open. The TUI also re-reads all stream metadata a few times per
  second, so enormous stores make it busier.
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
