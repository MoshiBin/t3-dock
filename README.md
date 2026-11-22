# t3-dock

Open-source host app that shows [T3 Code](https://github.com/pingdotgg/t3code) agent threads on the
screen keys of a MiraBox N3 and focuses a thread when you press its key. The stock firmware stays.

**Status: milestone 2 of 4.** All six keys show live threads. Milestone 1 was confirmed on real
hardware. Design, findings and milestones are in
[docs/HANDOFF.md](docs/HANDOFF.md).

## Build (Windows)

Needs Rust `stable-x86_64-pc-windows-gnullvm` and [llvm-mingw](https://github.com/mstorsjo/llvm-mingw)
(UCRT) on `PATH`. Visual Studio and admin rights are not needed. Rustup defaults to an MSVC host, so
name the host:

    winget install MartinStorsjo.LLVM-MinGW.UCRT --scope user
    winget install Rustlang.Rustup --override "-y --default-host x86_64-pc-windows-gnullvm"
    cargo build --release

A running t3-dock locks its exe, so run `t3-dock stop` before rebuilding.

## Use

    t3-dock auth    create or check the T3 token
    t3-dock         show threads on the N3
    t3-dock stop    stop the running instance and blank the keys

Close MiraBox's own app first.

`auth` asks T3's CLI for a 30-day token with only `orchestration:read`, labeled `t3-dock`, and saves it
encrypted for your Windows user (DPAPI). That scope can also read files and thread contents, so t3-dock
sends it only to loopback. Revoke it in T3 Code under Settings > Connections. After it expires, run
`t3-dock auth` again.

The log is `%LOCALAPPDATA%\t3-dock\t3-dock.log`. It has input codes, states and counts, and no titles,
ids, tokens or message text.

## Develop

    cargo test
    cargo test preview -- --ignored         render sample tiles to target/tiles.png
    cargo run --example probe -- out.png    open the N3 without writing to it

Third-party code is credited in [THIRD_PARTY.md](THIRD_PARTY.md).

Not affiliated with T3 Tools or MiraBox. MIT licensed.
