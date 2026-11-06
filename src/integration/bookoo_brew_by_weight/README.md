# BOOKOO Mini → Linux → Linea Mini brew-by-weight

**Experimental; not yet validated with a physical BOOKOO and Mini R.** The Linux
host reads a BOOKOO Themis Mini and presents a virtual `LUNAR-C` scale to the
Mini's native brew-by-weight system. The Mini performs the weight cutoff. The
bridge does not switch the machine into standby, start its pump, or use a relay.

This is an optional, separate integration. Keep the host and container running
throughout brewing. The adapter must reach both devices and support simultaneous
BLE central and peripheral roles. It uses the same read-only host D-Bus mount as
the ordinary BOOKOO integration; privileged Docker and host networking are not
required. BOOKOO Ultra is not included in this integration's compatibility claim.

## Linux setup

Install and enable BlueZ as described in the ordinary BOOKOO guide. Check:

```sh
busctl get-property org.bluez /org/bluez/hci0 org.bluez.Adapter1 Roles
```

The list must include `central-peripheral`. If the controller supports that
feature but it is disabled, enable only its kernel feature in the existing
`[General]` section of `/etc/bluetooth/main.conf`:

```ini
KernelExperimental = 671b10b5-42c0-4696-9227-eb28d1b049d6
```

Preserve any other explicitly enabled feature UUIDs. Restart `bluetooth.service`
while no Bluetooth device is in use, then check the roles again. Setting this
does not add capabilities the radio lacks. The bridge reports a setup error
rather than silently using an unsuitable adapter. Keep BlueZ enabled at boot.

## App setup and target weight

1. Remove the ordinary BOOKOO integration and any other client that maintains a
   Mini Bluetooth connection. Only one integration can own either device.
2. Put the BOOKOO in manual weighing/timer mode, in grams. Close other scale apps.
3. Under **Status → Machine integrations → +**, choose
   **BOOKOO → Mini brew-by-weight**. Scan/select the BOOKOO and Mini separately.
4. Supply the Mini's local Bluetooth token. The existing
   [pairing/token instructions](../bluetooth_scale/README.md#set-up) apply.
   No La Marzocco account password is sent to this integration.
5. Save. After receiving valid scale readings, the bridge advertises `LUNAR-C`.
   It briefly authenticates to the Mini and reads its scale registrations. If
   none exists, it requests registration with `force:false`, then releases its
   owner connection so the Mini can connect to the virtual scale. Another
   registered scale is never overwritten; remove it in the La Marzocco app first.
   Registration delivery does not prove that location calibration persisted.
6. In the **La Marzocco app**, check that `LUNAR-C` is connected, complete any
   required scale/location training, set Dose 1/2 yields and select the dose you
   want. For example, set Dose 1 to 36 g. A continuous dose does not stop by weight.
   Keep the Linux host in the same position after location training: the Mini
   measures the host's radio signal, not the BOOKOO's.

**Target yield belongs to the Mini.** This app's grinder recipe/reporting weight
is separate and does not program the Mini's dose. Set matching targets in both.
You may need the official app/cloud for dose configuration; live weight forwarding
and local scale registration make no cloud requests.

## Pulling a shot

1. Wait for the integration to show **Bridge ready**. Check the app's weight
   against the physical scale and a known weight before the first trial.
2. Grind with GbS and arm the grinder request as usual. Prepare the puck and cup.
3. Put the cup on the scale, then move the Mini paddle to brew. Its scale commands
   tare the BOOKOO and reset/start its timer. The bridge forwards actual weights.
4. The Mini should stop at its selected yield, using its native cutoff algorithm.
   Return its paddle to stop. Leave the cup until the final weight settles.
5. The bridge reports time between the Mini's scale timer commands and settled
   final yield to GbS. Timer-command time may differ from pump or shot-counter
   timing; this must be compared on hardware. Pre-infusion pauses cannot finalize
   a shot by themselves. Late drips count toward yield, not extraction time.

**Test shot** in the card arms reporting without adjusting the grinder. It still
requires a real Mini brew and can use native automatic stopping. Aborting the
app's test/report only cancels reporting: **stop the Mini with its paddle**.
Native brew-by-weight can work without an armed GbS request; unrequested shots
are never sent to the grinder.

## Failure behavior and validation

- Start with a supervised water-only test; stop with the paddle if the Mini does
  not stop at target. Compatibility, cutoff precision, calibration persistence
  and reconnection need testing on the actual machine/scale firmware.
- Stale/corrupt BOOKOO readings, lost subscriptions, command timeouts and queue
  overload stop forwarding and disconnect the virtual scale. Do not assume the
  Mini stops its pump on link loss; use its paddle. No cached weight or invented
  zero is forwarded as a fresh measurement.
- BOOKOO has no documented dedicated command ACK. Tare confirmation observes a
  new weight within ±0.10 g; reset observes zero timer; start observes timer
  advancement; stop observes an unchanged timer across at least 200 ms of fresh
  notifications. These observations do not prove causation. Control confirmation
  expires after two seconds; measurement freshness expires after one second.
- An interruption during a brew or outstanding scale command latches the bridge
  off. Stop the Mini, then open Edit and Save the integration to reconnect. Idle
  disconnections retry automatically. Reconnecting discards any active GbS
  measurement. It does not tare, start a timer, or reconstruct a partial shot.
  Re-arm a new shot after recovery.
- Reporting failures (such as cup removal before the final yield settles) do not
  interrupt a healthy native scale link. Enter that shot manually.
- The bridge reserves the scale, Mini and virtual-scale identity against competing
  integrations in the same app. Close external clients yourself.

CLI equivalents: `--bookoo-bbw-address`, `--bookoo-bbw-adapter`,
`--bookoo-bbw-mini-address` and `--bookoo-bbw-mini-token`. Prefer the UI password
field to placing a secret in shell history. Existing ordinary BOOKOO settings
keep their previous behavior; this bridge must be added explicitly.

The virtual scale protocol is adapted from the Apache-2.0
[kaspizzo/lamarzocco bridge](https://github.com/kaspizzo/lamarzocco/blob/main/docs/controller/SCALE_ADAPTER.md).
Its BOOKOO path also has synthetic tests only. Attribution and license are in
[third-party/scale-bridge](../../../third-party/scale-bridge/NOTICE).
BOOKOO frames follow its [published Mini protocol](https://github.com/BooKooCode/OpenSource/blob/main/bookoo_mini_scale/protocols.md).
