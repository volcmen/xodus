# Forza social integration proposal

## Status and scope

This local branch is a scoped proposal and coordination reference. It is not an
accepted upstream protocol, does not replace a general friends/chat UI, and has
not been installed or validated with a game.

- Official `xodus/main` base: `a92abacb0743f16c769279b9057c8c28435e5ef9`
- Local reviewed range: `a92abacb0743f16c769279b9057c8c28435e5ef9..315c343b9412f6cae9bae7f0c49c4b597b5a0502`
- Local source tip: `315c343b9412f6cae9bae7f0c49c4b597b5a0502`
- Task 5 feature tip: `40e76edc822c5880ad9f32b2daae69f15659751e`

The range contains title-scoped authorization, Forza PeopleHub and Multiplayer
Activity operations, bounded local social/activation protocols, a normal
desktop overlay, and service-owned overlay process management. The game-facing
`xgameruntime` bridge is planned and reviewed separately; it is not part of
this Xodus branch.

The reviewed source commits are:

| Commit | Scope |
| --- | --- |
| `baa811b1d6d08b0cdd7e73a6a202d9b9e6616aec` | Title-scoped Xbox authorization |
| `c4491c0ce8107b79de284062163191008772346c` | Authorization privacy hardening |
| `4e297f65144a5d958e905e19adb9ba7927165a3c` | PeopleHub and Multiplayer Activity operations |
| `b42de8d81e5b9b0f44a2fbe12b74e143d01ab00c` | Activity and PeopleHub contract hardening |
| `778c25ae90278b600e804f32b7aee4839b3fec8d` | Bounded social and activation IPC |
| `eaaec55ea49b7c0b650429164c7c15260487eb10` | Keyboard/controller social overlay |
| `c202927a56215321064e98ccbd995533e3d7b2e8` | Overlay lifecycle and reentrancy hardening |
| `40e76edc822c5880ad9f32b2daae69f15659751e` | Service-owned XDUI launcher |
| `fb38a3f7ad6c9112e8779f7908ffdcc2b2468f30` | Keyring-free shared social protocol crate |
| `57091ce69c467d95fd27b4e9aecaca34c7eaf7ac` | PeopleHub fallback, authorization lifetime, and bounded social operations |
| `2b7982a523d6135e6ad4d8f9ca6cf1c877d26047` | Cancellation-safe session-cache invalidation |
| `903fc4843d25cbfa84db262e41d29a7ca4f8e319` | Bounded activation delivery and fail-closed socket permissions |
| `08c995b389f2bbc5d0dbc9d51bcadf1453368266` | Shared overlay in-flight gate and capacity-one request queue |
| `315c343b9412f6cae9bae7f0c49c4b597b5a0502` | Versioned full/invite-only XDUI request and acknowledgement |

The range also contains maintainer documentation commits
`966cb58586db725a1d1201a34457eb2e4a62f33f` and
`e8bcd8fcfc473e7ad019c3ee8d32841ee6f98fd3`; neither is a source change.

## Ownership and trust boundary

`xodus-service` owns every privileged or transport-sensitive operation:

- title-scoped Xbox authorization and request signing;
- PeopleHub friend/presence and Multiplayer Activity query/invite operations;
- the private mapping from short-lived opaque friend handles to transport
  identities;
- validation and one-subscriber relay of invite activation URIs; and
- discovery, single-instance launch, cancellation, and reaping of the overlay.

`xodus-social-protocol` owns only the redacted XDSO model and bounded framing.
It depends directly on `serde`, `serde_json`, `thiserror`, `tokio`, and `uuid`.
The `xodus` library re-exports this contract through the existing
`xodus::forza::ipc::*` and `xodus::forza::{...}` public paths.

`xodus-overlay` owns presentation and input only. It consumes
`xodus-social-protocol` directly rather than the full `xodus` library, receives
display labels, coarse presence, opaque handles, and fixed status text, and
does not receive a raw XUID, authorization token, connection string, or
activation URI.

The service accepts connections only after restricting the socket to exactly
`0600`. A permission-restriction failure closes the listener, removes the
socket, and fails startup rather than leaving a misleading service running.
The router checks Unix peer credentials for the same UID before constructing a
connection context or dispatching XML, protobuf, XDSO, XDSI, XDAP, or XDUI
work. Social/activation errors and public evidence use fixed categories or
synthetic placeholders; transport identifiers and credentials are not logged.

## Local protocols

All multibyte integer fields below are little-endian.

### XDSO — social request/response

`xodus-social-protocol` provides the shared model, encoder, reader, and
serialized writer. Each request and response is one bounded frame:

| Field | Width | Contract |
| --- | ---: | --- |
| Magic | 4 bytes | `XDSO` |
| Version | `u16` | `1` |
| Payload length | `u32` | JSON payload bytes, at most 1 MiB |
| Payload | variable | Exactly the declared byte count |

The reader rejects bad magic/version, truncation, length mismatch, invalid
JSON, and lengths above 1 MiB before allocating the payload. `FrameWriter`
serializes each complete encoded frame behind one mutex, so concurrent writers
cannot interleave headers or payloads.

Serde's externally tagged variants are the operation contracts:

| Direction | Operation | Payload/result |
| --- | --- | --- |
| Request | `Snapshot` | No payload; return the cached redacted snapshot. |
| Request | `RefreshFriends` | No payload; refresh through the service backend and replace the snapshot. |
| Request | `InviteFriend` | One opaque `FriendHandle`. |
| Request | `JoinFriend` | One opaque `FriendHandle`. |
| Response | `Snapshot` | Display labels, coarse `Online`/`Offline`/`Unknown` presence, opaque handles, and fixed status text. |
| Response | `Success` | Fixed action status text. |
| Response | `Error` | Fixed safe error text. |

Every successful refresh generates a new service-private handle map and
replaces the prior map. A handle therefore lives only for the current snapshot;
using an old or unknown handle returns the fixed stale-selection error without
calling PeopleHub or Multiplayer Activity.

Each refresh, invite, or join operation has one 30-second bound covering both
the shared social-service mutex wait and backend I/O. A timeout drops the
operation future, releases the mutex, cancels pending backend I/O, and returns
only a fixed safe error. The service does not automatically retry a social
operation.

### XDSI — activation subscriber

After the `XDSI` magic, one same-UID client may register as the activation
subscriber. The service sends repeated frames consisting of `u32 length`
followed by exactly that many UTF-8 URI bytes. Each URI is capped at 4 KiB.
The subscriber queue holds at most one pending activation; nonblocking
backpressure rejects another activation without evicting a healthy subscriber.
Service cancellation, EOF, or I/O failure unregisters the subscriber.

### XDAP — activation publisher

After the `XDAP` magic, the publisher sends one `u32 length` and one UTF-8
activation URI, also capped at 4 KiB. The service validates the expected
activation scheme/host and the exact required query fields before publishing
to the registered subscriber. It replies with one byte: `0` published, `1`
rejected or unavailable. The relay independently requires a nonempty URI of at
most 4096 bytes before enqueue, including for URIs constructed by XDSO Join.
XDSO Join reports success only after that URI is accepted by the capacity-one
relay.

### XDUI — overlay request

After the `XDUI` magic, a version-1 requester sends exactly two bytes:

| Field | Width | Contract |
| --- | ---: | --- |
| Version | `u8` | `1` |
| Mode | `u8` | `0` full social, `1` Multiplayer Activity invite-only |

The service reads the complete request under a two-second deadline that is also
interruptible by service cancellation. Unknown, incomplete, or timed-out
requests are rejected without starting a child. Capacity, cancellation, path,
and spawn failures are likewise pre-acknowledgement rejections. A rejection
writes only terminal byte `2` followed by EOF.

After validation and child spawn, the service writes acknowledgement byte
`0xa1`. Only then does it wait for a terminal outcome and write exactly one
terminal status byte:

| Status | Meaning |
| ---: | --- |
| `0` | The requested overlay action completed. In invite-only mode this is possible only after a matching Invite request succeeds. |
| `1` | The user deliberately cancelled. |
| `2` | Busy, path/spawn/process/protocol failure, unexpected exit, signal, disconnect cleanup, or service shutdown. |

The launcher uses a nonblocking shared gate. A nonempty `XODUS_SOCIAL_UI`
selects an explicit executable path; otherwise the service uses the
`xodus-overlay` sibling of its own executable. The child receives only the fixed
arguments `--mode full` or `--mode invite-only` and no explicit environment
additions. Cancellation, peer disconnect, unexpected requester input, or an
acknowledgement-write failure kills and then waits for the child before
releasing the gate. Normal exits are also always waited. No user-interaction
timeout is imposed after acknowledgement.

Version 1 deliberately breaks the old payload-free exchange. An old client
that sends only `XDUI` times out, receives one `2`, and never launches a child.
A new client does not accept an old service's terminal `0`, `1`, or `2` as the
required `0xa1` acknowledgement; it closes, allowing the old service to reap
its child. Both directions are covered by offline compatibility tests.

## Service API contract

The service mints title-scoped authorization internally and signs requests to
the Xbox PeopleHub and Multiplayer Activity services. At a high level it:

1. retrieves the authenticated account's friends and coarse presence;
2. queries published activity for the Forza title;
3. sends an invite using the authenticated account's published activity; or
4. validates a selected friend's joinable activity and constructs an
   activation URI for the private relay.

Endpoint paths, title claims, contract versions, and response validation are
owned by `xodus-service`/the `xodus` library. The overlay and future
game-facing bridge do not own credentials or transport identifiers.

PeopleHub transport records keep `displayName` and `gamertag` separate. A
nonempty `displayName` is preferred; a nonempty `gamertag` is the fallback; a
record with neither is rejected. Title authorization expiry is preserved in
the cached Forza session. A session that is expired or within 60 seconds of
expiry is discarded and reminted before use. Any PeopleHub or Multiplayer
Activity failure, including cancellation or timeout while the session is in
use, invalidates the cache so a later request can remint. The failed or
cancelled operation itself is never automatically retried.

## Overlay and input

The overlay is a normal GTK4/libadwaita application window. SDL3 is linked
dynamically through `pkg-config`; no vendored SDL build is enabled. The overlay
dependency tree contains neither the full `xodus` crate nor a keyring or Secret
Service backend. The overlay/launcher also adds no compositor command,
layer-shell protocol, global input grab, Xbox RTA registration, or background
friend/invite network polling.

Keyboard and Xbox-style controller inputs reach the same semantic dispatcher:

| Action | Keyboard | Controller |
| --- | --- | --- |
| Move up/down | Arrow keys or `W`/`S` | D-pad or left stick with engage/neutral hysteresis |
| Activate selected row | Enter or Space | South button |
| Cancel/back | Escape or Backspace | East button |
| Invite | `I` | West button |
| Join | `J` | North button |
| Refresh | `R` or F5 | Start button |

Refresh, Invite, Join, and selected-row activation share one model-owned
in-flight gate across button, row, keyboard, and controller paths. While busy,
repeated network actions are ignored and the corresponding controls and rows
are rendered inactive; navigation and back/cancel remain available. The gate
clears on a response or client/request failure, while success retains the
existing close behavior. The nonblocking model-to-client queue has capacity
one, so it cannot accumulate repeated operations behind the active request.

Controller hotplug state tolerates zero devices, resets stick hysteresis when
the active device changes, and joins its listener on window close. The XDSO
client worker likewise cancels and joins on close.

Full mode retains Invite and Join. Invite-only mode hides and disables Join;
`J` and the controller North button cannot dispatch it. Row activation,
Enter/Space, and the controller South button all dispatch Invite in that mode,
while `I` and the controller West button remain explicit Invite controls. The
model binds a successful response to its pending action, so an unsolicited or
Refresh-associated success cannot produce exit status `0`.

## Upstream coordination

As verified on 2026-09-01:

- [Issue 94](https://github.com/xodus-gaming/xodus/issues/94) is the broader
  presence/friend/chat request. This proposal implements only the Forza
  friend/presence, invite, and join slice; chat is unsupported.
- [PR 105](https://github.com/xodus-gaming/xodus/pull/105) is an initial
  `xgameui` profile experiment whose player-list and close behavior remain
  future work. This branch is a separate normal-window reference, not a claim
  to supersede that work.
- [PR 108](https://github.com/xodus-gaming/xodus/pull/108) proposes
  title-claimed XSTS over IPC. This branch re-derived only the title
  authorization needed by its service-owned Forza path using current XML
  conventions. It does not claim PR 108 acceptance.

No maintainer approval, public branch, or pull request is implied.

## Safe source reproduction

The automated source gate used cached dependencies only and did not start the
service or overlay:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --offline -- -D warnings
git diff --exit-code a92abacb0743f16c769279b9057c8c28435e5ef9..HEAD -- crates/xodus-cli/src/commands/streaming.rs
! cargo tree -p xodus-overlay --offline | rg 'keyring|secret-service'
! rg -n 'use xodus::|xodus::' crates/xodus-overlay
cargo clippy -p xodus-social-protocol -p xodus -p xodus-service -p xodus-overlay --all-targets --offline -- -D warnings
cargo test -p xodus-social-protocol --offline
cargo test -p xodus --offline -- --skip test_get_xbox_live_dev_token
cargo test -p xodus-service --offline
cargo test -p xodus-overlay --offline
cargo test --workspace --offline -- --skip test_get_xbox_live_dev_token
cargo build --release -p xodus-service -p xodus-cli -p xodus-overlay --offline
rg -n 'Authorization:|XBL3\.0|ms-xbl-multiplayer://inviteAccept\?.+' docs crates || true
git diff --check
```

Current results:

- Formatting: **GREEN**.
- Full-workspace Clippy: **BLOCKED BY UNCHANGED UPSTREAM BASELINE**. The
  unchanged `xodus-cli` streaming helpers have 11 and 10 arguments versus
  Clippy's default limit of 7. The base-to-tip diff for that file is empty; no
  suppression or out-of-scope source edit was made.
- The dependency-tree reproduction before extraction found
  `dbus-secret-service-keyring-store`, `dbus-secret-service`, and
  `keyring-core` below `xodus-overlay`. After extraction, both the dependency
  scan and the overlay `xodus::` source scan return no matches: **GREEN**.
- `xodus-social-protocol` has exactly five direct library dependencies:
  `serde`, `serde_json`, `thiserror`, `tokio`, and `uuid`.
- Strict all-target Clippy for every changed package
  (`xodus-social-protocol`, `xodus`, `xodus-service`, and `xodus-overlay`):
  **GREEN**, no issues.
- Protocol compatibility tests: **GREEN**, 6 passed. They include a literal
  version-1 `Snapshot` frame assertion in addition to the moved boundary and
  concurrency tests.
- Filtered workspace tests: **GREEN**, 117 passed, 1 ignored, 1 filtered out
  across 16 suites. The filtered test is `test_get_xbox_live_dev_token`, an
  upstream anonymous live Microsoft test.
- Offline release build for `xodus-service`, `xodus-cli`, and `xodus-overlay`:
  **GREEN**.
- Privacy scan: 15 final-tree matches, all existing authorization format
  strings, static documentation templates, synthetic test/protocol data, or
  the literal scan expression itself. No nickname or live identifier was
  recorded.

Privacy match classification:

- `docs/xbox/xboxservices.md` and `docs/xbox/gameruntime.md`: static
  documentation templates with visible placeholders.
- `docs/forza-social.md`: the literal safe reproduction search expression.
- `crates/xodus/src/tokens.rs` and `crates/xodus/src/api/xbox/auth.rs`:
  production header format strings whose runtime values are not present in
  source.
- `crates/xodus/tests/forza.rs`, `crates/xodus/src/forza/api.rs`,
  `crates/xodus-service/src/invite_activation.rs`,
  `crates/xodus-service/src/social_ipc.rs`, and the remaining token tests:
  visibly synthetic sentinels and protocol-shape assertions.

## Manual matrix and limitations

| Check | Status |
| --- | --- |
| Keyboard navigation/action parity | **NOT RUN** |
| Physical Xbox controller navigation/hotplug | **NOT RUN** |
| Invite from a published Private Multiplayer lobby | **NOT RUN** |
| Join a compatible remote activity | **NOT RUN** |
| Deliberate cancellation and exit `1` | **NOT RUN** |
| Peer disconnect/service shutdown cleanup | **NOT RUN** |
| Automatic incoming invite notification | **NOT SUPPORTED** |

Chat, automatic incoming notifications, and Xbox RTA are explicitly
unsupported. The manual game path remains gated on the separately reviewed
`xgameruntime` bridge, installation with artifact/source hash parity, and
explicit authorization for live account/game validation.
