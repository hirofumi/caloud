# caloud

A wrapper for `claude` that adds Notification Center support and `say`-based voice notifications.

> [!NOTE]
>
> It's likely unnecessary now that Claude Code v1.0.38 supports [hooks](https://docs.anthropic.com/en/docs/claude-code/hooks).

## Prerequisites

- macOS
- Rust
- `preferredNotifChannel` must be `iterm2` or `iterm2_with_bell` (set via `/config` → Notifications)

## Installation

```bash
git clone https://github.com/hirofumi/caloud.git
cd caloud
cargo install --locked --path .
```

## Usage

```bash
caloud [OPTIONS] -- [CLAUDE_PATH] [CLAUDE_ARGS...]
```

### Options

- `--say=<ARGS>`: Enable voice notifications with `say` command arguments
  - Example: `--say='-v Samantha -r 200'`
  - If not specified, voice notifications are disabled
- `--line-wrap=<MODE>`: Control line wrapping adjustment (default: `preserve`)
  - `adjust`: Rejoin URLs split by `claude`'s line wrapping using heuristics
  - `preserve`: Keep original line breaks as-is
