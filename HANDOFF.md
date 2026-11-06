# Bluetooth scales and Linea Mini session handoff

Recorded 7 November 2026 for the work performed on 6–7 November. This document
captures the implementation, operating decisions, validation and recovery work
from the session. It is suitable for sharing with the source. The separate
private handoff retains the original host inventory, logs and backups.

**The implementation is committed and running in Docker, but no physical BOOKOO
scale and Linea Mini shot has been tested.** Hardware compatibility and automatic
cutoff accuracy remain unverified. The next step is supervised hardware validation.

## Repository and deployment state

- Upstream baseline: `d35cd3a680537a83dd62fe8e9ab7b6abaaadd915`, release v0.4.0.
- Implementation commit: `b59316e7e04d6033a4cae847e660a8f7b83a8cd4`.
- Working and default branch of the personal repository:
  `feature/bluetooth-scales`. Upstream history is preserved.
- `origin` points to the personal private repository; `upstream` retains the
  original project. The implementation was pushed and its remote commit verified.
  No release tags or upstream pull request were created.
- GitHub Actions is disabled in the personal repository. Review the inherited
  nightly image/release workflows before enabling it; their source was not changed.
- The session deployment uses an ARM64 Linux host, container `gbs-anywhere`,
  image `gbs-anywhere:bluetooth-local`, and application version `0.4.0+bbwbr01`.
  This is a locally built image; upstream v0.4.0 does not contain these additions.
- Docker publishes port 80, uses restart policy `unless-stopped`, runs as UID
  65532, and mounts the named volume `gbs-anywhere` at `/data`. Host `/run/dbus`
  is mounted read-only. Neither privileged mode nor host networking is required.
- Docker and Bluetooth services are enabled at boot. At implementation handoff,
  there were no configured integrations, no active shot and no real Mini scale
  registration. Recheck live state before acting on that snapshot.

The documentation addition does not rebuild or replace the running image.
Source in GitHub does not back up the Docker data volume, host configuration,
credentials or rollback images.

## Scope and decisions from the session

The initial work reviewed the existing project, its grinder workflow and its
La Marzocco cloud authentication, then built and deployed it on Linux. The
requested machine is a Linea Mini R, model year 2026; its exact firmware was not
provided. No scale was available for physical testing.

The chosen approach uses the existing Linux host and Bluetooth. No relay,
machine wiring, GPIO service or extra microcontroller was added. Development
progressed through direct scale reporting, Mini paddle timing, and a separate
native brew-by-weight bridge.

- Added direct BOOKOO Themis Mini BLE support, device discovery, live weight,
  explicit scale tests, reconnect handling and manual reporting fallback.
- Reviewed the Bluetooth fork during development. The resulting usability
  features are described here; this branch does not include an Eureka driver
  or claim complete feature parity with that fork.
- Added optional Mini paddle detection and a local token-reading command.
- Added TIMEMORE support earlier, then removed it at the user's request.
  `timemore_bluetooth` and its flags are absent. Old saved entries can appear
  unsupported and be deleted in the UI. Historical local artifacts may still
  mention TIMEMORE; they do not describe current support.
- Added the separate experimental BOOKOO-to-Mini native brew-by-weight bridge.
- Updated the UI, English/German text, documentation, tests and packaging.
  Shared shot detection was extracted from Half Decent for reuse.

The requested coffee-by-weight reference used Python, Raspberry Pi hardware
and a relay. It was researched, not installed or directly transplanted. The
implemented virtual-scale bridge instead adapts an Apache-2.0 ESP32/C reference
to Rust and Linux BlueZ. Its provenance is recorded in
[the third-party notice](third-party/scale-bridge/NOTICE), with the original
license alongside it. Keep both files in source and distributed builds.

## Three distinct BOOKOO workflows

Grind-by-Sync uses the completed shot's time and yield to let the grinder adjust
its grind setting. The grinder communicates with this app over the existing
network API; it does not use the Mini's Bluetooth connection.

| Mode | Tare and timing | Who stops the machine? |
|---|---|---|
| Ordinary BOOKOO, no Mini address | Tare when the grinder request/test arms; time from first drops to flow stop | The operator |
| Ordinary BOOKOO with Mini paddle timing | A fresh Mini brew-start event tares the scale and starts the app timer; brew stop freezes it | The operator |
| BOOKOO → Mini brew-by-weight | Mini scale commands tare/reset/start/stop the BOOKOO; reporting uses Mini timer-command times | The Mini's native selected-dose cutoff, subject to hardware validation |

The ordinary driver sends tare but does not control the scale's physical timer.
The bridge forwards the Mini's timer controls as well. Timer-command timing,
shot-counter timing and first-drop timing are different definitions; set the
grinder's target time consistently with the selected mode.

Use one automatic reporter for a shot. Do not configure the ordinary BOOKOO
driver and the bridge against the same scale or Mini. The app rejects conflicting
device ownership; close other phone apps and external Bluetooth clients too.
La Marzocco cloud reporting can also compete with scale reports: the first
accepted result wins.

Only BOOKOO Themis Mini is covered by the new driver's intended compatibility.
BOOKOO Ultra, TIMEMORE, Eureka and arbitrary devices discovered by a scan are
not covered by that claim. Even the intended Mini/scale combination still needs
physical validation. Existing La Marzocco cloud and Half Decent integrations
remain available.

## Setup and normal use

Start with the existing guides, which contain the full commands and UI fields:

- [BOOKOO scale and Linux/Docker setup](src/integration/bookoo_bluetooth/README.md)
- [Mini pairing, local token and paddle timing](src/integration/bluetooth_scale/README.md)
- [Native brew-by-weight setup and target yield](src/integration/bookoo_brew_by_weight/README.md)
- [Grinder pairing and general setup](README.md)

BlueZ runs on the host. The container uses
`DBUS_SYSTEM_BUS_ADDRESS=unix:path=/run/dbus/system_bus_socket` and the D-Bus
mount. A read-only mount still permits D-Bus calls; the host's policy and numeric
user identity must allow the container to use BlueZ. See the BOOKOO guide if
UID 65532 needs a locked host identity. Preserve existing users and policy.

The direct scale driver connects to the selected address; PIN pairing and
`bluetoothctl trust` are not required. The UI scans fill the selected address
field, and saving the integration initiates connection. Scan results can include
cached or unnamed devices. Manufacturer-data bytes are not readable device names,
and the app cannot manufacture a name that the device does not advertise.

For the Mini, follow the linked pairing procedure and read its local token using
`--read-mini-token`. The command prints a secret locally and exits. Store it in
the integration's password field. Subsequent connections reuse the saved token;
a reset or authentication failure may require obtaining it again. Optional
paddle timing checks the actual shot-counter service rather than assuming a
model year or firmware version establishes support.

The bridge requires the adapter's `Roles` property to include
`central-peripheral`. The session enabled the following specific feature under
`[General]` in the existing `/etc/bluetooth/main.conf`:

```ini
KernelExperimental = 671b10b5-42c0-4696-9227-eb28d1b049d6
```

Preserve other enabled UUIDs and settings. This enables a supported controller
feature; it cannot give an unsupported radio new capabilities. Keep Bluetooth
and Docker enabled, and verify adapter power and application recovery after a
reboot. A phone's Bluetooth adapter cannot replace the host's adapter.

### Using native brew-by-weight

1. Add **BOOKOO → Mini brew-by-weight** under **Status → Machine integrations → +**.
   Select the scale and Mini, provide the local Mini token, and save.
2. The host connects to the BOOKOO and advertises a virtual `LUNAR-C` scale.
   It briefly authenticates to the Mini for registration, then releases that
   connection so the Mini can connect to the host's virtual scale. This mode
   does not keep the ordinary paddle-notification connection open.
3. Registration reads the existing scale configuration and uses `force:false`.
   It does not overwrite another registered scale. Remove an unwanted existing
   scale through the official app first. Delivery of a registration request
   alone does not prove pairing or location calibration succeeded.
4. In the La Marzocco app, verify the scale connection, complete any location
   training, set Dose 1/2 yields and select a weight-based dose. For example,
   select a 36 g dose. Continuous mode does not provide a weight cutoff.
5. Set the grinder's recipe/reporting target separately. This project's target
   weight does not program the Mini's dose; keep the two targets consistent.
6. Wait for **Bridge ready**, check weight against the real scale, then grind
   with GbS. On E64 WS, press the knob when prompted; on E65/E80, the completed
   GbS grind arms the request. Prepare the puck and put the cup on the scale.
7. Start with the Mini paddle. Its scale commands should tare and start the
   timer. The Mini should stop at the selected yield. Return the paddle to stop
   and leave the cup until final weight settles so the app can report the shot.

Keep Linux and the container running throughout brewing. Location training
measures the host's radio, so moving the host can affect it. Live forwarding
and local registration use no cloud account login; the official app/cloud may
still be needed to configure the Mini's doses and location training.

Native Mini scale commands are forwarded even without an armed grinder request.
Only an explicitly armed grinder shot or selected test can produce a report.
**Test shot** never adjusts the grinder. In the two Mini modes it waits for a
real Mini brew; a pour alone does not start it. In ordinary first-drop mode,
it can be checked by pouring water after the test tares the scale.

## Timing and failure behavior

The grinder request starts the reporting deadline, not the extraction timer.
Preparation, extraction and final-weight settling must all fit inside that
window. A reporting timeout or **Abort test** does not operate the pump.

| Timing or condition | Implemented behavior |
|---|---|
| Shot reporting window | Default 180 s from the grinder request; configurable in the app. Valid finite settings are 30–3600 s; 0 disables the deadline. |
| Result finishing hold | Default 6 s so the grinder can observe the result; separate from extraction time. |
| First-drop detection | Weight rises at least 1 g over baseline and stays above the threshold for 1 s; start is backdated to the last baseline reading. |
| First-drop flow stop | Less than 0.5 g rise over 3 s, with minimum duration/yield rules; see the shared detector for cup-lift handling. |
| Mini final yield | Requires a stop event and at least 3 s of settling afterward. Late drips affect yield, not extraction time. |
| Mini reporting tare | Requires a reading within ±0.5 g of zero within 1.5 s of start. |
| Automatic Mini reporting minimums | At least 5 s and 5 g; yield must also reach half the recipe weight when supplied. |
| Ordinary BLE interruption | A reading gap over 2 s invalidates the measurement. Reconnection never resumes a partial shot. |
| Bridge measurement freshness | 1 s; stale values are not forwarded as new measurements. |
| Bridge control confirmation | 2 s. Tare observes a fresh reading within ±0.10 g; reset/start/stop observe timer behavior. Stop requires at least 200 ms of fresh unchanged timer readings. |

BOOKOO does not provide a dedicated command acknowledgement in the implemented
protocol. Observed weight/timer changes are confirmation heuristics, not proof
that a command caused the change. The 2 s bridge command deadline and 1.5 s
reporting-zero deadline protect different operations.

The bridge stops forwarding and disconnects the virtual scale for corrupt/stale
readings, lost subscriptions, command timeouts or queue overload. A failure
during a brew or pending scale command latches it off. **Stop the Mini with its
paddle**, then Edit/Save the integration to reconnect and arm a new shot. Idle
disconnects retry automatically. Do not assume the Mini stops pumping when its
scale link disappears.

A reporting-only failure, such as lifting the cup before final yield settles,
does not tear down a healthy native scale link. Use manual entry for that shot.
Neither mode reconstructs a partial measurement after reconnect. Subscription
replays and mid-shot updates cannot trigger a fresh paddle-mode tare.

## Credentials and storage

The existing [cloud integration](src/integration/la_marzocco/README.md) sends the
configured email/password over HTTPS to the vendor's default authentication
endpoint in [cloud.rs](src/integration/la_marzocco/cloud.rs). It obtains and
refreshes access tokens. The current integration has no API-key replacement
setting. Its configured credentials are saved in the local owner-readable
settings file; this is filesystem protection, not encrypted storage.

The added Bluetooth modes use a local Mini token and need no cloud email/password.
Secrets entered in the app are not returned by its settings API. Avoid putting
tokens in command arguments, logs, screenshots or commits. The explicit token
reader intentionally prints its result only when invoked by the operator.
The `/data` volume and future backups can contain credentials and pairing data.

No real cloud credentials or Mini registration were configured during the
implementation session. GitHub CLI was subsequently installed and authenticated
for publication; that login is separate from the app and stays outside Git.

## Implementation map

| Files | Responsibility |
|---|---|
| [bluetooth_scale](src/integration/bluetooth_scale/) | Shared BlueZ transport/discovery, Mini authentication/events, paddle capture and ordinary scale workflow |
| [bookoo_bluetooth](src/integration/bookoo_bluetooth/) | BOOKOO settings, packet parsing, tare and ordinary integration |
| [bookoo_brew_by_weight](src/integration/bookoo_brew_by_weight/) | Virtual GATT scale, Mini registration, source connection, command confirmation, fault handling and independent GbS recording |
| [scale_shot.rs](src/integration/scale_shot.rs), [scale_live.rs](src/integration/scale_live.rs) | Shared shot detection and app readings; Half Decent reuses the detector |
| [integration/mod.rs](src/integration/mod.rs), [manager.rs](src/integration/manager.rs) | Registration, shot/test identity and device ownership |
| [control_api.rs](src/server/control_api.rs), [bluetooth_tests.rs](src/server/bluetooth_tests.rs) | Scan/test/live-reading API and HTTP coverage |
| [machine.rs](src/protocol/machine.rs) | Grinder-facing reporting, including manual tests that must not adjust the grinder |
| [main.rs](src/main.rs) | Local Mini token CLI path |
| [web](web/) | Scan selectors, live readings, tests, manual fallback and English/German text |
| [Dockerfile](Dockerfile), [Cargo.toml](Cargo.toml), [packaging](packaging/) | Pure-Rust Linux `zbus` dependency, D-Bus environment and third-party notices in image/package/tarball |

Read [AGENTS.md](AGENTS.md) before changing code. Preserve exact shot identity,
grinder wire compatibility, secret redaction, bounded queues and selected-peer
checks. The bridge must not gain pump, standby or relay commands as an incidental
fix to a scale problem.

## Validation completed and remaining

The implementation source was checked against the tested backup before its
initial commit; it was unchanged. Recorded results:

- 280 automated tests passed: 273 library and 7 binary tests. The normal suite
  had one ignored, explicitly opt-in host smoke test.
- Formatting, strict Clippy, documentation with warnings denied, and the Rust
  1.88.0 minimum-version check passed.
- Protocol, simulated BlueZ/GATT, replay/reconnect, command timeout, shot/test
  identity and manual fallback paths were exercised.
- Real host and non-root Docker GATT advertisement probes passed. They used a
  temporary probe advertisement and cleaned it up; they did not pair a Mini.
- Phone-width UI checks covered English/German and light/dark modes, including
  setup forms and ordinary BOOKOO/bridge states.
- The private cleanup tool had 17 passing tests using temporary fixtures and
  mocked Docker/APT mutations. Dry runs and backup checks passed; source archive
  extraction was compared with the checkout. This was not a live recovery drill.

Run the project checks again for future implementation changes:

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --locked
cargo test --locked
cargo +1.88.0 check --all-targets --locked
```

The simulated Bluetooth tests need `dbus-daemon`. UI changes also require the
phone checks in AGENTS.md. Build outputs were removed after validation to save
space, so the next build will regenerate them. This handoff-only change needs
documentation/link checks and does not require recreating those build outputs.

Before relying on automatic cutoff, validate on the actual machine and scale:

1. Record firmware versions privately and compare fresh weight against known
   weights, including tare and timer behavior.
2. Confirm Mini pairing, registration, selected dose and location calibration
   survive reconnects and power cycles. Verify adapter roles after host reboot.
3. Perform a supervised water-only extraction, ready to stop with the paddle.
   Check tare/timer ordering, latency and actual cutoff versus target weight.
4. Compare first-drop, paddle/shot-counter and bridge timer-command durations,
   including pre-infusion and late drips; choose the appropriate GbS target time.
5. Check disconnection and stale-reading recovery, exact-shot reporting, tests
   and manual entry. Confirm no old result can answer a later grinder request.

Neither protocol tests nor successful Linux advertising proves those physical
behaviors. The implementation remains experimental until they are demonstrated.

## Cleanup, backups and recovery

The private companion bundle is stored outside the checkout in a sibling
`gbs-anywhere-handoff` directory on the session host. It contains its own
`HANDOFF.md`, `cleanup.py`, frozen `inventory.json`, `test_cleanup.py`, reports
and protected backups. It is deliberately not copied into this repository:
it contains host identities and may contain secrets. A fresh clone does not
include that bundle or a portable copy of its cleanup script.

The script defaults to a dry run. It checks the original inventory checksum,
host/Docker identities and selected paths before deletion; changed targets,
symlinks and active consumers can cause refusal. Back up first and review the
exact plan. Do not edit its inventory to defeat a refusal or run it against an
unrelated host. Git commits, remote changes and this new document mean its old
checkout fingerprint no longer matches; review and make a fresh backup before
any source deletion. A partial cleanup is possible if a later action fails.

Cleanup coverage recorded in that private bundle:

| Category | What is covered and what to preserve |
|---|---|
| Rust build output | `target/debug`, `release`, `msrv`, `doc` and compiler metadata. This group was explicitly removed after validation; the running image was unaffected. |
| Session artifacts | Logs, screenshots, preview/build helpers, research checkouts, old TIMEMORE archives and Bluetooth scan output. These contain evidence or local identifiers; review before discarding. |
| Shared caches/tools | Cargo/rustup, pip/APT caches, browser dependencies, unused BuildKit cache and recorded stale sockets. Require separate review; they may serve other work. |
| Docker candidates/rollbacks | Candidate tags and older stopped versions. Image layers can be shared, so tag sizes are not additive reclaimed space. Keep the latest pre-bridge rollback until it is no longer needed. |
| Bluetooth/launcher changes | Restore only the session's specific configuration changes when retiring the bridge. Preserve unrelated host settings and clients. |
| Full uninstall | Current container/image, data volume, checkout and launcher are separate explicit selections. Preserve verified backups before removing source or data. |
| GitHub CLI | Installed after the frozen inventory. Sign out with `gh auth logout --hostname github.com` if intended; review CLI package removal separately. Do not publish its credential directory. |

Only the Rust build-output group was applied as the requested space cleanup.
Temporary swap, test browser environments and probe containers/advertisements
had also been removed after their individual test rounds. Source, app data,
toolchains, Cargo caches and rollback containers were retained. No general
Docker volume prune, package autoremove or blanket host-log cleanup was performed.

Protected archives include source plus Git history and formerly untracked files,
the app volume, small session evidence, host configuration copies and checksums.
The initial bundle excludes Docker image archives, large compiler caches and
whole research checkouts; its script has an explicit image-backup option.
Copy backups to private storage before retiring the host. Validate checksums,
inspect archives and restore into a fresh directory first. Stop the app before
restoring its volume and preserve UID/GID 65532 ownership.

The immediate pre-bridge fallback is container `gbs-anywhere-before-bbw` and
image `gbs-anywhere:before-bbw` (application `0.4.0+bookoo1`). It retains ordinary
BOOKOO and optional paddle timing, with TIMEMORE removed. Stop the Mini, back up
current data, remove a configured bridge before using an older build, and follow
the private handoff's rollback procedure. Rollback containers share the same
data volume: they revert code, not settings/history. One older pre-UI container's
original image was unavailable; do not assume every historical rollback starts.

When retiring the bridge, remove any subsequently created `LUNAR-C` registration
through the La Marzocco app. Linux file cleanup cannot undo machine-side pairing
or calibration. Forget only a verified intended Bluetooth device, and preserve
the host's shared BlueZ device database and Docker services.
