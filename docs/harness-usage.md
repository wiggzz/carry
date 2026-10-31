# Install and use the Carry harness

[Back to Carry](../README.md)

### One-line install

```sh
curl -fsSL https://raw.githubusercontent.com/wiggzz/carry/main/install.sh | sh
```

The installer detects Linux x86_64 or macOS Intel/Apple Silicon, downloads the
matching latest release, verifies its SHA-256 checksum, and installs to
`~/.local/bin`. No Rust toolchain is required. It prints a PATH hint if needed;
set `CARRY_INSTALL_DIR` for the shell running the script to choose another directory.
See the macOS notes below if Gatekeeper blocks the unsigned binary.

### Install a release

Choose the archive for your system from the
[latest release](https://github.com/wiggzz/carry/releases/latest) and download its
`SHA256SUMS` alongside it. macOS binaries are available starting with the first
release containing this change; older releases contain only Linux binaries.

| System | Archive suffix |
| --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-gnu.tar.gz` |
| macOS Apple Silicon (`uname -m` reports `arm64`) | `aarch64-apple-darwin.tar.gz` |
| macOS Intel (`uname -m` reports `x86_64`) | `x86_64-apple-darwin.tar.gz` |

#### macOS quick start

In Terminal, change to the download directory. Set `archive` to the exact file
you downloaded (replace `<version>`, including its `v` prefix):

```sh
archive='carry-<version>-aarch64-apple-darwin.tar.gz'
# Intel Macs: use carry-<version>-x86_64-apple-darwin.tar.gz instead.
grep -F "  $archive" SHA256SUMS | shasum -a 256 -c - &&
  tar -xzf "$archive" &&
  mkdir -p "$HOME/.local/bin" &&
  install -m 0755 carry "$HOME/.local/bin/carry"
export PATH="$HOME/.local/bin:$PATH"
carry --help
```

The checksum command must report `OK` before installation. Add the `export PATH`
line to `~/.zshrc` to keep Carry on your PATH in new Terminal windows.
The macOS binaries are built and smoke-tested on macOS 15 for each architecture;
older macOS versions are not tested. They are not Developer ID signed or
notarized. If Gatekeeper blocks a verified download, follow Apple's
[Open Anyway instructions](https://support.apple.com/en-us/102445) in System
Settings → Privacy & Security, or build from source below.

To run a first task, set `OPENAI_API_KEY` or use `carry login` as described under
[Run an isolated task](#run-an-isolated-task), then run:

```sh
carry --cwd /path/to/disposable/repo -p 'Explain how this project is organized'
```

Tasks use your configured model provider and may incur usage costs. The
`carry --help` check above does not call a model.

#### Linux quick start

```sh
archive='carry-<version>-x86_64-unknown-linux-gnu.tar.gz'
grep -F "  $archive" SHA256SUMS | sha256sum -c - &&
  tar -xzf "$archive" &&
  mkdir -p "$HOME/.local/bin" &&
  install -m 0755 carry "$HOME/.local/bin/carry"
export PATH="$HOME/.local/bin:$PATH"
carry --help
```

### Build from source

```sh
git clone https://github.com/wiggzz/carry.git
cd carry
cargo build --release --locked
./target/release/carry --help
```

### Run an isolated task

Set an API key in the process environment and point Carry at a disposable
checkout:

```sh
export OPENAI_API_KEY=...
carry --cwd /path/to/disposable/repo fix the failing tests
```

An eligible ChatGPT subscription can use the Codex endpoint instead:

```sh
carry login
# headless host:
carry login --device-auth
```

`OPENAI_API_KEY` takes precedence and is required for a custom
`OPENAI_BASE_URL`. Remove the stored subscription credential with `carry logout`.
Use `-p` when the prompt begins with an option-like value.

Run without a prompt to start the localhost-only HTML UI and open it in your
default browser. Use `--no-open` on remote/headless machines or to open the printed
URL yourself. Browser-launch failures do not stop the server.
`--serve` remains an explicit alias. Use `--interactive` for terminal follow-ups
and steering. Supplying a prompt still runs once in the terminal; use `--print`
to explicitly select one-shot mode, including when piping a prompt from stdin:

```sh
carry --cwd ../project
carry --port 9000
carry --interactive -p "investigate the flaky test"
printf 'explain the failing tests' | carry --print
```

### Terminal input and formatting

In `--interactive` mode, Enter sends the draft. Alt+Enter or Ctrl+J inserts a
newline; Shift+Enter also works when the terminal reports it distinctly. Pasted
blocks are inserted without submitting (bracketed paste), preserving indentation
and blank lines. Arrow keys edit the draft, including earlier lines. Ctrl+C clears
the draft; Ctrl+D on empty input or `/quit` exits. `/help` lists commands. The old
`/paste` and `/end` commands remain available for compatibility.

The editor stays in the normal terminal screen. Status output is printed above
the editable draft. While the editor is active, streamed answers are buffered to
complete lines (the last partial line is flushed on completion) to avoid disruptive
per-token redraws. Terminal shell commands are shown once when execution starts,
not repeatedly while their arguments are being generated; the browser still shows
live shell-call previews. On Unix, run `cargo build && python3 tests/terminal_editor.py`
for the editor’s pseudo-terminal smoke tests.

Reedline is used only when stdin, stdout, and stderr are terminals. If either output
stream is redirected, Carry uses the compatible line-oriented reader (`/paste`,
`/end`, and `/cancel` remain available) so terminal protocol bytes never enter
redirected stdout.

Answers use lightweight terminal Markdown styling: colored headings, bold text,
inline/fenced code, and muted block quotes. Lists, links, and tables remain readable
Markdown source; code is colored as a block, not syntax-highlighted. Redirected
stdout, `NO_COLOR`, and `TERM=dumb` preserve the original Markdown without styling.
Output stays in native terminal scrollback, without a live token-counter redraw.
Your terminal's “scroll on output” setting controls whether new output scrolls
you back down.

Model answer/commentary text is previewed as it streams, in both the browser
and a terminal (on stderr when it is a TTY). Complete streamed lines receive the
same lightweight Markdown styling as completed answers. Previews are provisional;
a fully streamed terminal answer is not printed a second time at completion.
If streaming was incomplete or stdout is redirected, the complete answer is still
printed; redirected stdout remains suitable for piping. Tool arguments and context bookkeeping are not displayed
as raw JSON. Private reasoning is not shown.

Each model response in the browser includes its own input, cached-input, output,
and total token counts. The footer shows only estimated session cost in USD, using the repository’s
benchmark rates (currently available for `gpt-5.6-luna`). This is not a billing
quote. Missing pricing or older responses without cost metadata show compact token totals instead
(e.g. `2.5m tokens (2.4m cached, 120k out)`), rather than an incomplete cost.

In the browser, submitted messages stay beside the composer as “sending”, then
“queued” once accepted. They move into the conversation when added to the model's
context, rather than appearing among output before they have been consumed.
Steering received during a finishing response starts another turn automatically.

## Inspect a run

Sessions live under `$CARRY_HOME/sessions` (or `~/.carry/sessions`). Continue a
session by ID or path while writing the continuation elsewhere:

```sh
carry --resume 20260827-123456-abcdef --session-home /path/to/carry-home -p "continue"
carry --resume /path/to/prior-session --session-dir /path/to/fresh-output -p "new task"
```

Each session includes:

- `context-state.json` — atomically updated canonical context checkpoint
- `final.patch` — the agent's resulting patch
- `result.json` — outcome, usage, cost estimate, retries, and compactions
- `trace.jsonl` and shell-output files — chronological execution evidence

The default model for ordinary Carry runs is `gpt-6-sol`; choose another with
`--model` or `OPENAI_MODEL`. SWE-bench benchmark entry points separately default
to `gpt-6-luna` and pass that choice explicitly to Carry, so changing Carry's
interactive default does not change benchmark costs. There is no default step
limit; use `--max-steps N` only when a specific cap is required. Shell commands
default to a 60-second wall-clock timeout. Set the session fallback with
`--default-shell-timeout-secs N`. The model can set `timeout_secs` on an
individual `shell` call to 1–300 seconds for work that needs a different limit;
`null` uses the session default. A timed-out command and its process group are
killed.
