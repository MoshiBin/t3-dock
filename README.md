# t3-dock

Open-source host app that shows [T3 Code](https://github.com/pingdotgg/t3code) agent threads on the
screen keys of a MiraBox N3 and focuses a thread when you press its key. The stock firmware stays.

**Status: scaffold.** Design, findings and milestones are in [docs/HANDOFF.md](docs/HANDOFF.md).

## Build (Windows)

Needs Rust `stable-x86_64-pc-windows-gnullvm` and [llvm-mingw](https://github.com/mstorsjo/llvm-mingw)
(UCRT) on `PATH`. Visual Studio is not needed.

    cargo build --release
    cargo run --example probe -- target/tiles.png

The probe finds and opens the N3 without writing to it, and renders sample tiles to a PNG.

Not affiliated with T3 Tools or MiraBox. MIT licensed.
