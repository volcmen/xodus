# Forza social integration proposal

## Status and scope

This local branch is a scoped proposal and coordination reference. It is not an
accepted upstream protocol, does not replace a general friends/chat UI, and has
not been installed or validated with a game.

- Official `xodus/main` base: `a92abacb0743f16c769279b9057c8c28435e5ef9`
- Local source range: `a92abacb0743f16c769279b9057c8c28435e5ef9..40e76edc822c5880ad9f32b2daae69f15659751e`
- Local Task 5 tip: `40e76edc822c5880ad9f32b2daae69f15659751e`

The range contains title-scoped authorization, Forza PeopleHub and Multiplayer
Activity operations, bounded local social/activation protocols, a normal
desktop overlay, and service-owned overlay process management. The game-facing
`xgameruntime` bridge is planned and reviewed separately; it is not part of
this Xodus branch.

## Ownership and trust boundary

`xodus-service` owns every privileged or transport-sensitive operation:

- title-scoped Xbox authorization and request signing;
- PeopleHub friend/presence and Multiplayer Activity query/invite operations;
- the private mapping from short-lived opaque friend handles to transport
  identities;
- validation and one-subscriber relay of invite activation URIs; and
- discovery, single-instance launch, cancellation, and reaping of the overlay.

`xodus-overlay` owns presentation and input only. It receives display labels,
coarse presence, opaque handles, and fixed status text. It does not receive a
raw XUID, authorization token, connection string, or activation URI.

The service socket is created with mode `0600`. The router checks Unix peer
credentials for the same UID before constructing a connection context or
dispatching XML, protobuf, XDSO, XDSI, XDAP, or XDUI work. Social/activation
errors and public evidence use fixed categories or synthetic placeholders;
transport identifiers and credentials are not logged.

## Local protocols

All multibyte integer fields below are little-endian.

### XDSO — social request/response

Each request and response is one bounded frame:

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

### XDSI — activation subscriber

After the `XDSI` magic, one same-UID client may register as the activation
subscriber. The service sends repeated frames consisting of `u32 length`
followed by exactly that many UTF-8 URI bytes. Each URI is capped at 4 KiB.
Service cancellation, EOF, or I/O failure unregisters the subscriber.

### XDAP — activation publisher

After the `XDAP` magic, the publisher sends one `u32 length` and one UTF-8
activation URI, also capped at 4 KiB. The service validates the expected
activation scheme/host and the exact required query fields before publishing
to the registered subscriber. It replies with one byte: `0` published, `1`
rejected or unavailable.

### XDUI — overlay request

After the `XDUI` magic, the requester sends no payload. The service launches at
most one overlay and replies with exactly one status byte followed by EOF:

| Status | Meaning |
| ---: | --- |
| `0` | The overlay completed an invite or join action. |
| `1` | The user deliberately cancelled. |
| `2` | Busy, path/spawn/process/protocol failure, unexpected exit, signal, disconnect cleanup, or service shutdown. |

The launcher uses a nonblocking shared gate. A nonempty `XODUS_SOCIAL_UI`
selects an explicit executable path; otherwise the service uses the
`xodus-overlay` sibling of its own executable. The child receives zero
arguments and no explicit environment additions. Cancellation, peer
disconnect, or unexpected requester input kills and then waits for the child
before releasing the gate. Normal exits are also always waited. No user
interaction timeout is imposed.

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

## Overlay and input

The overlay is a normal GTK4/libadwaita application window. SDL3 is linked
dynamically through `pkg-config`; no vendored SDL build is enabled. The
overlay/launcher adds no GNOME Keyring dependency, compositor command,
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

Controller hotplug state tolerates zero devices, resets stick hysteresis when
the active device changes, and joins its listener on window close. The XDSO
client worker likewise cancels and joins on close.

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
cargo clippy -p xodus -p xodus-service -p xodus-overlay --all-targets --offline -- -D warnings
cargo test --workspace --offline -- --skip test_get_xbox_live_dev_token
cargo build --release --workspace --offline
rg -n 'Authorization:|XBL3\.0|ms-xbl-multiplayer://inviteAccept\?.+' docs crates || true
git diff --check
```

Current results:

- Formatting: **GREEN**.
- Full-workspace Clippy: **BLOCKED BY UNCHANGED UPSTREAM BASELINE**. The
  unchanged `xodus-cli` streaming helpers have 11 and 10 arguments versus
  Clippy's default limit of 7. The base-to-tip diff for that file is empty; no
  suppression or out-of-scope source edit was made.
- Strict all-target Clippy for `xodus`, `xodus-service`, and `xodus-overlay`:
  **GREEN**, no issues.
- Filtered workspace tests: **GREEN**, 85 passed, 1 ignored, 1 filtered out
  across 14 suites. The filtered test is `test_get_xbox_live_dev_token`, an
  upstream anonymous live Microsoft test.
- Offline release workspace build: **GREEN**, 505 crates compiled.
- Privacy scan: 13 final-tree matches, all classified below; no live value.

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
