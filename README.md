# caloud

A wrapper for `claude` that adds Notification Center support and `say`-based voice notifications.

> [!NOTE]
>
> It's likely unnecessary now that Claude Code v1.0.38 supports [hooks](https://docs.anthropic.com/en/docs/claude-code/hooks).

## Prerequisites

- macOS
- Rust
- `claude` binary available in your `$PATH`
- `preferredNotifChannel` must be `ghostty` (set via `/config` → Notifications)

## Installation

```bash
git clone https://github.com/hirofumi/caloud.git
cd caloud
cargo install --path .
```

## Usage

```bash
caloud <claude-options>
```
