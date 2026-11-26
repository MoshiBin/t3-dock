# t3-dock

Open-source host app that shows [T3 Code](https://github.com/pingdotgg/t3code) agent threads on the
screen keys of a MiraBox Stream Dock N3 or 293S V3 and focuses a thread when you press its key. The
stock firmware stays.

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
      [new]  [needs you]  [settle]

By default:

| Control | Turn | Press |
|---|---|---|
| Top dial | Scroll by a row | Home, all projects |
| Bottom-left dial | Brightness | Reset brightness to 60% |
| Bottom-right dial | Show one project's threads | Show project names while held |

The keys under the screens:

- **New** opens a new thread in the project T3 has open (T3's Ctrl+N).
- **Needs you** opens the next thread that needs you, from any project or page.
- **Settle** settles the thread T3 shows, or unsettles it if it is settled (T3's Ctrl+Shift+S). If T3
  is not in front, the first press only brings it forward, so you can see which thread it would settle.

New and settle press T3's default shortcuts, so they don't work if you changed those, or while T3's
terminal has focus. They are typed only while T3 is in front.

### Stream Dock 293S V3

    +--------+--------+--------+--------+--------+  +-----------+
    | thread | thread | thread | thread | thread |  | needs you |
    +--------+--------+--------+--------+--------+  +-----------+
    | thread | thread | thread | thread | thread |  |  working  |
    +--------+--------+--------+--------+--------+  +-----------+
    | thread | thread | thread | thread | thread |  |   more    |
    +--------+--------+--------+--------+--------+  +-----------+

All 15 keys are threads. The side display can't be pressed: it counts the threads that need you, the
ones working, and those that didn't get a key. Notices such as "Connecting" show at its top instead
of on key 1. The 293S has no other keys or dials, so the bindings in the settings window apply only to
the N3. If both are plugged in, t3-dock uses the N3.

**Settings** in the tray menu opens a window where you can give the three keys and the three dials
other actions, reverse a dial, and change preferences: the reset brightness, the brightness per
dial click, how long messages show, hiding idle threads and start at login. Save applies them to the
running dock at once. The screen keys always open their thread. Settings are kept in
`%LOCALAPPDATA%\t3-dock\config.json`.

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

The tray menu shows the connection state, opens the settings, sets up or renews the token, opens the
log, turns start at login on or off (off by default) and quits. The same exe has three subcommands:

    t3-dock settings   open the settings window
    t3-dock auth       create or check the T3 tokens
    t3-dock stop       stop the running instance and blank the keys

It is a GUI program, so PowerShell doesn't wait for it. Add `| Out-Host` to wait.

`auth` and the tray item ask T3's CLI for a 30-day token with only `orchestration:read`, labeled
`t3-dock`, and save it encrypted for your Windows user (DPAPI). That scope can also read files and
thread contents, so t3-dock sends it only to loopback. Revoke it in T3 Code under Settings >
Connections. A working token is kept, so renewing does something only once it has expired.

### Remote environments

Threads from every remote environment that T3 Code's desktop client has saved over SSH show up
too, mixed in with local ones. A press opens the thread in its own environment. Holding the
bottom-right dial shows the environment's label after the project name.

t3-dock reads the list from the client's encrypted connection catalog, taking only the SSH targets
and never the client's own credentials. It then joins the T3 server that the client already runs
on each host. It never starts or stops a remote server. Each remote needs a token of its own, so
`auth` and the tray item also run the remote's T3 CLI over SSH. That issues the same
`orchestration:read` token, which then reaches the server only through an SSH port forward to its
loopback port. SSH runs with `BatchMode=yes`, so key or agent login must work, as it does for the
client. Remotes saved over a relay or a direct URL are skipped. The tray shows how many remotes are
connected.

The log is `%LOCALAPPDATA%\t3-dock\t3-dock.log`. It has input codes, states and counts, and no titles,
ids, tokens or message text.

## Develop

    cargo test
    cargo test preview -- --ignored         render sample tiles to target/tiles.png
    cargo test read_survives -- --ignored   HID regression test: needs a Stream Dock, stop t3-dock first
    cargo test show_numbers -- --ignored --nocapture
                                            number every key and print presses, to check the order
    cargo run --example probe -- out.png    open the N3 without writing to it

Third-party code is credited in [THIRD_PARTY.md](THIRD_PARTY.md).

Not affiliated with T3 Tools or MiraBox. MIT licensed.
