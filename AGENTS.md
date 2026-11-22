# t3-dock

Rust host app: T3 Code threads on a MiraBox N3, press a key to focus the thread.
Read `docs/HANDOFF.md` before working on USB, the T3 protocol or focus.

- MIT project, intended to be public. Do not copy GPL code (opendeck-akp03, deck-for-t3code, OpenDeck).
- Never commit or log tokens, WebSocket tickets, environment ids, device serials, real thread titles
  or message text. The shell stream contains message text. Fixtures use invented data.
- T3 access is `orchestration:read` only and loopback only.
- Never flash firmware or write the boot logo.
- Before committing: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
