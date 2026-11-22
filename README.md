# t3-dock

Open-source host app that shows [T3 Code](https://github.com/pingdotgg/t3code) agent threads on the
screen keys of a MiraBox N3 and focuses a thread when you press its key. The stock firmware stays.

**Status: early, and working on real hardware.** All four milestones are written and have been used on
an N3 with T3 Code 0.1.11 on Windows 11. The T3 stream it reads is not a stable API. Some paths, such
as sleep and USB replug, are untested: see [docs/HANDOFF.md](docs/HANDOFF.md).

## Build (Windows)

Needs Rust `stable-x86_64-pc-windows-gnullvm` and [llvm-mingw](https://github.com/mstorsjo/llvm-mingw)
(UCRT) on `PATH`. Visual Studio and admin rights are not needed. Rustup defaults to an MSVC host, so
name the host:

    winget install MartinStorsjo.LLVM-MinGW.UCRT --scope user
    winget install Rustlang.Rustup --override "-y --default-host x86_64-pc-windows-gnullvm"
    cargo build --release

A running t3-dock locks its exe, so run `t3-dock stop` before rebuilding.

## Use

Close MiraBox's own app, then start `t3-dock.exe`. It has no window, only a tray icon.

    +--------+--------+--------+
    | thread | thread | thread |   press: open that thread in T3 Code
    +--------+--------+--------+
    | thread | thread | thread |
    +--------+--------+--------+
     [prev]  [needs you]  [next]

| Control | Turn | Press |
|---|---|---|
| Top dial | Scroll by a row | Home, all projects |
| Bottom-left dial | Brightness | Reset brightness to 60% |
| Bottom-right dial | Show one project's threads | Show project names while held |

The middle key under the screens opens the next thread that needs you, from any project or page.

| Color | Meaning |
|---|---|
| Amber | Needs you: approval, question or plan |
| Blue | Starting, working or waiting |
| Green | Finished since you last opened it |
| Red | Failed or hit a usage limit |
| Gray | Idle, or only monitoring |

On the first page an active thread keeps its key. An idle one gives its key to a thread that becomes
active, and a finished one also to a thread that needs you. Other pages don't change while you look at
them. Subagents fold into their parent.

The tray menu shows the connection state, sets up or renews the token, opens the log, turns start at
login on or off (off by default) and quits. The same exe has two subcommands:

    t3-dock auth    create or check the T3 token
    t3-dock stop    stop the running instance and blank the keys

It is a GUI program, so PowerShell doesn't wait for it. Add `| Out-Host` to wait.

`auth` and the tray item ask T3's CLI for a 30-day token with only `orchestration:read`, labeled
`t3-dock`, and save it encrypted for your Windows user (DPAPI). That scope can also read files and
thread contents, so t3-dock sends it only to loopback. Revoke it in T3 Code under Settings >
Connections. A working token is kept, so renewing does something only once it has expired.

The log is `%LOCALAPPDATA%\t3-dock\t3-dock.log`. It has input codes, states and counts, and no titles,
ids, tokens or message text.

## Develop

    cargo test
    cargo test preview -- --ignored         render sample tiles to target/tiles.png
    cargo test read_survives -- --ignored   HID regression test: needs an N3, stop t3-dock first
    cargo run --example probe -- out.png    open the N3 without writing to it

Third-party code is credited in [THIRD_PARTY.md](THIRD_PARTY.md).

Not affiliated with T3 Tools or MiraBox. MIT licensed.
