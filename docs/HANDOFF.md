# Handoff

Researched 2026-11-22 against T3 Code 0.1.11 (orchestration protocol 2) on Windows 11.
Each section says whether it was **tested**, **read from source**, or is a **proposal**.

## Goal

See T3 Code agents on the N3's screen keys. Press one to focus that agent's thread in T3 Code.

## Decisions

- Our own host app on stock firmware. The T3 deck plugins found that can focus threads are macOS-only,
  the Windows-capable one only shows counts, and OpenDeck's N3 plugin declares "zero effort" Windows support.
- Rust, with `gnullvm` + llvm-mingw.
- MIT. `mirajazz` (MPL-2.0) is an unmodified dependency.

## Tested

- `mirajazz` 0.16.3, `cosmic-text` 0.19, `tokio-tungstenite` 0.30 and `tray-icon` 0.26 build with Rust 1.100.0 gnullvm + llvm-mingw 20261117.
- `mirajazz` finds and opens the N3.
- `tokio-tungstenite` reaches `ws://127.0.0.1:3773/ws` (401 without a ticket, as expected).
- `cosmic-text` renders mixed Hebrew/English correctly at 64x64. About four short lines fit.
- A release exe with those crates runs without the toolchain on PATH.

### Confirmed by the user on the hardware, 2026-11-22 (milestone 1)

- Image rotation and key order.
- Every input code in the table below, with down and up for keys and dial presses.
- Deep-link navigation to a thread with a UUID id, with T3 in front, covered and minimized.
- The left dial reported twice for each requested click, in both directions. The other dials reported
  once. It is unknown whether that is the dial or the hand.

### Exercised by the user on the hardware, 2026-11-22 (milestones 2-4)

The user approved the result as a whole. These are the parts the log shows being used:

- Six live keys, paging with the buttons (since replaced by new and settle), and scrolling with the
  top dial.
- Project filter, project names while held, and turning the brightness dial. Deliberate single turns
  of that dial reported once this time.
- Focus as it is now: from behind another window T3 was in front after 64 and 295 ms, with no retries.
- No HID disconnects during the session.

### Tested from the host side

- User-scope toolchain: rustup stable gnullvm (1.100.0) and winget llvm-mingw.
- A token issued through the CLI has exactly `orchestration:read`.
- The authenticated stream: snapshot, acks, updates after the first ack, heartbeats.
- Recovery from a T3 restart on another port and from a rejected token, against a stand-in server
  (`src/t3/recovery_tests.rs`).
- HID reads survive redraws: 8 of 8 runs, against 2 of 3 failing before the fix (see the traps).
- Single instance, graceful stop, starting with T3 unreachable, the GUI subsystem, subcommand output,
  and the tray icon registering with Explorer.

## Not yet tested

- The new-thread and settle buttons. Not yet pressed on the hardware.
- The needs-you button with a thread actually waiting. It was pressed only while none was.
- Pressing the top dial away from home, and pressing the bottom-left dial.
- Focus with T3 minimized since it raises first. Navigation to thread ids that contain `:` was not
  confirmed separately.
- The tray menu by hand, start at login (only its registry helpers, on a test key) and renewing a
  token from the tray.
- A real T3 restart, USB replug, and sleep and resume.

## Hardware

Tested, from descriptors:

- `6603:1003`, product string `HOTSPOTEKUSB HID DEMO`.
- Control interface MI_00: usage page 65440, usage 1. Report sizes including the id byte: in 513,
  out 1025, feature 0. MI_01 is a keyboard interface that Windows owns: ignore it.

Read from the mirajazz source and, for images and input, confirmed on the device. See also MiraBox's MIT SDK
(`Python-SDK/src/StreamDock/Devices/StreamDockN3.py`):

- Protocol v3: `Device::connect(info, 3, 9, 3)`.
- Images: JPEG, 64x64, rotated 90 degrees clockwise (`ImageRotation::Rot90`), no mirroring.
- Input reports start with `ACK`. Byte 9 is the code, byte 10 the state (1 down, 0 up).

| Control (hex codes) | Codes |
|---|---|
| Screen keys, top-left to bottom-right | `01`..`06` |
| Keys without screens | `25`, `30`, `31` |
| Left dial: left, right, press | `90`, `91`, `33` |
| Top dial: left, right, press | `50`, `51`, `35` |
| Right dial: left, right, press | `60`, `61`, `34` |

mirajazz traps:

- Image key indices are 0-based. The library adds 1 on the wire.
- `set_button_image` only caches. Nothing appears until `flush()`.
- Don't call `set_button_image`: it uses `block_in_place`. That retires the runtime thread 10 s later,
  and Windows cancels any overlapped read that thread issued (`0x800703E3`), dropping the connection.
  `src/device.rs` encodes the JPEG itself, calls `write_image`, and keeps all HID I/O on one thread.
- Call `keep_alive()` periodically. opendeck-akp03 uses 15 s.
- `ButtonStateChange` is a whole-state vector. Building one from a single event loses other held keys,
  so we decode raw reports and emit our own events.
- The firmware version is always `None` on Windows (mirajazz issue 10).
- Don't run MiraBox's own app at the same time.
- Initializing sets brightness to 0, so set it after every connect.

## T3 Code protocol

Read from the installed 0.1.11 bundle and beastyrabbit/opendeck-t3-code-status (MIT, dc27f17).
A maintainer recommends this stream in pingdotgg/t3code#10929, but it is not a stable API.

**The field names in that issue's first answer are V1** (`hasPendingApprovals`, `session.status`,
`latestTurn`, `thread-upserted`). Do not use them. V2 is below.

1. Origin: `origin` in `~/.t3/userdata/server-runtime.json`. Require loopback.
2. `GET /.well-known/t3/environment` (no auth) returns `environmentId`,
   `orchestrationProtocolVersion` (must be 2) and `capabilities`.
3. Credential: `~/.t3/bin/t3.cmd auth session issue --scope orchestration:read --label t3-dock --ttl 30d --token-only`.
   - Always pass `--scope`: the default is nearly full access.
   - The app captures stdout itself, so the token reaches no terminal, log or agent transcript.
   - Store it with DPAPI and handle expiry.
   - This scope can also read files and thread contents.
4. `POST /api/auth/websocket-ticket` with the bearer token and body `{}` returns `{ticket}`
   (about 5 minutes).
5. Connect to `/ws?orchestrationProtocol=2&wsTicket=<ticket>`. Never log this URL.
6. Frames are JSON, either one object or an array of objects:
   - Send `{"_tag":"Request","id":"shell","tag":"orchestration.subscribeShell","payload":{},"headers":[]}`.
   - For each `{"_tag":"Chunk","requestId":"shell","values":[...]}` send
     `{"_tag":"Ack","requestId":"shell"}`. The server waits for it before sending more.
   - Send `{"_tag":"Ping"}` about every 30 s and expect `Pong`. Answer a received `Ping` with `Pong`.
   - `Exit` ends the request. Reconnect with a new ticket and take a fresh snapshot.
   - The payload may set `omitIdleSubagents` if `capabilities.shellThreadFamilies`, and
     `omitOlderSettled` if `capabilities.shellOlderSettled`.

Stream items, by `kind`:

- `snapshot`: `snapshot.{snapshotSequence, threads, archivedThreads, projects}`
- `thread.updated`: `sequence, location ("active"|"archive"), thread`
- `thread.removed`: `sequence, location, threadId`
- `project.updated`: `sequence, project`
- `project.removed`: `sequence, projectId`
- `synchronized`, `snapshot-required`

Thread fields we need. Parse leniently and ignore the rest:

- Identity: `id, projectId, title, creationSource, lineage.{parentThreadId, relationshipToParent, sideChat}`
- Run: `status, activityRunStatus, latestRunId, activeProviderThreadId, blockedOnThreadWait,
  pendingBackgroundTasks[].{kind, awaited}, lastErrorClass, limitRecovery`
- Needs you: `pendingRuntimeRequest.{kind, blocking}, pendingGuestMessageCount`, and
  `hasActionableProposedPlan` when `interactionMode == "plan"`
- Unread: `latestRunCompletedAt > lastVisitedAt`
- Visibility and order: `archivedAt, deletedAt, settledOverride, snoozedUntil, snoozedAt, pinnedAt, updatedAt`

Read from the server bundle: updates are coalesced per thread and sorted by sequence, so dropping
`sequence <= last` is safe. A `snapshot` with `resolvedRepositoryIdentityRoots` is only a project
refresh with an empty thread list: don't apply it.

`pendingRuntimeRequest.kind` is `user_input`, `auth_refresh`, or otherwise an approval.
`latestVisibleMessage.text` is conversation content: do not deserialize it.

beastyrabbit's `src/status.ts` mirrors the T3 sidebar's rules, including parked runs and subagent
approvals bubbling up to the parent. It is MIT, so it can be ported with attribution.

## Focus

Read from the installed desktop bundle, then fired on the hardware:

- `t3code://threads/<environmentId>/<threadId>` goes through the second-instance handler to the renderer's
  router, then activates the window. Percent-encode both parts: thread ids may contain `:`.
- Open it with ShellExecuteW, never `cmd /c start`, because the ids come from the server.
- The Windows foreground lock applies, because a HID report is not user input to Windows.

Found while building:

- T3's link handler only does a plain window activate. Measured with `ForegroundLockTimeout` at its
  maximum: the link alone raised a minimized T3 in 271 ms but never a covered one within 1.5 s, and
  the user pressed again while waiting. So `src/focus.rs` raises first (`AttachThreadInput` +
  `SetForegroundWindow`), then opens the link, then retries until T3 is in front. One of the two
  measured raise calls returned false, hence the retries.
- New thread and settle have no link, and the token is read-only, so they are T3's default
  shortcuts from the 0.1.11 bundle: `mod+n` is `chat.new` and `mod+shift+s` is `thread.settle`
  (toggles), both `when: !terminalFocus`. `t3code://new` needs a `prompt` and asks to confirm, so it
  can't make an empty thread. `src/focus.rs` sends the keys with `SendInput` only after checking that
  T3 is in front, and settle only if T3 was in front before the press.
- `lastVisitedAt` did not change when a press opened an already read thread, so it can't confirm
  navigation.
- The link launches a second T3 process with our environment. Inside a T3 agent shell that has
  `T3CODE_*`, and possibly `ELECTRON_RUN_AS_NODE`, so `main` removes those before anything starts.

## UX

    +--------+--------+--------+
    | agent  | agent  | agent  |  title + status color
    +--------+--------+--------+  amber needs you, blue working, green done, red failed
    | agent  | agent  | agent  |  press = focus that thread
    +--------+--------+--------+
      [new]  [needs you]  [settle]
     dials: top = scroll, bottom-left = brightness, bottom-right = filter by project

- A thread keeps its key while active, so nothing moves under the user's finger.
- Subagents and side chats fold into their parent, as in T3's sidebar
  (`relationshipToParent != "subagent" && sideChat != true` is a top-level thread). A side chat's
  approval or question shows on its direct parent, even a settled one.
- The title fills a key, so the project shows while the bottom-right dial is held.
- Keys move only at home (`src/board.rs`). A page the user scrolled to is frozen.

## Milestones

1. **Vertical spike:** one real thread on one key, and pressing it focuses that thread. Settles rotation,
   key index, input codes, auth and foreground behavior. Needs the user to look and press.
2. All six keys live: status rules, stable slots, recovery from T3 restarts and USB replug.
3. The three buttons and three dials.
4. Tray app: no console, single instance, quit, optional start at login (ask before enabling).

## References

- https://github.com/4ndv/mirajazz af31624 (MPL-2.0)
- https://github.com/MiraboxSpace/StreamDock-Device-SDK fc124cb (MIT)
- https://github.com/beastyrabbit/opendeck-t3-code-status dc27f17 (MIT)
- https://github.com/4ndv/opendeck-akp03 10d1834 (GPL-3.0: facts only, no code)
- https://github.com/hsalazr/deck-for-t3code 8e962a7 (GPL-3.0: facts only, no code)
