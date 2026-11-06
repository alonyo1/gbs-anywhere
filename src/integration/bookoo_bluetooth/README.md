# BOOKOO Themis Mini over Bluetooth

Reports cup weight and shot duration directly from a BOOKOO Themis Mini,
using the Linux computer's Bluetooth adapter. No La Marzocco account or
internet connection is needed. Stop the espresso machine yourself: this
integration does not provide the Mini's automatic Brew-by-Weight cutoff.

This implementation follows [BOOKOO's published Mini Scale protocol](https://github.com/BooKooCode/OpenSource/blob/main/bookoo_mini_scale/protocols.md).
Packet parsing and shot handling have automated tests. Real-scale testing
is still needed; other BOOKOO models and other Bluetooth scales are not
claimed as supported.

The Bluetooth transport test uses a private `dbus-daemon` (install the
`dbus-daemon` package on a development machine); it sends no test traffic
to the host's BlueZ service or to a real scale.

## Set up

These experimental drivers are not in the upstream v0.4.0 image. Build an
image from this checkout (`docker build -t gbs-anywhere:bluetooth .`) and
use that image when creating the container, instead of the upstream image.

1. On the Linux host, install BlueZ and enable its service:

   ```sh
   sudo apt-get install bluez
   sudo systemctl enable --now bluetooth
   bluetoothctl power on
   ```

2. Turn the scale on, use its manual weighing mode with grams, and close
   phone apps connected to it. Find its address on the host:

   ```sh
   bluetoothctl
   scan le
   # Note the address next to your BOOKOO scale.
   scan off
   quit
   ```

   No PIN pairing or `trust` command is needed. Only the address you choose
   is connected; the integration never selects an arbitrary nearby scale.

3. For Docker, share the host's system D-Bus socket. Add this mount to the
   usual `docker run`, preserving the existing data volume and port mapping:

   ```sh
   -v /run/dbus:/run/dbus:ro
   ```

   BlueZ runs on the host. The container keeps its non-root user; neither
   `--privileged`, raw Bluetooth device access nor host networking is needed.
   The host's D-Bus policy must permit that user to call `org.bluez`.
   Read-only mounting prevents filesystem changes but still allows D-Bus
   calls; mount it only for a container you trust.

   The image sets `DBUS_SYSTEM_BUS_ADDRESS` to
   `unix:path=/run/dbus/system_bus_socket`. If you use a different image,
   set that variable explicitly: distroless may have no `/var/run` alias.

   Some hosts also reject D-Bus clients whose numeric user ID has no host
   account. The image uses UID/GID **65532**. Check `getent passwd 65532`
   and `getent group 65532`. If both are unused, register a locked,
   no-login account for that identity:

   ```sh
   sudo groupadd --gid 65532 gbs-anywhere
   sudo useradd --uid 65532 --gid 65532 --no-create-home \
     --shell /usr/sbin/nologin gbs-anywhere
   sudo systemctl reload dbus
   ```

   Reuse an existing identity if that ID is already registered; do not
   change another account's ID. Running the container as root is not needed.

4. In **Status → Machine integrations → + → BOOKOO Themis Mini**, use
   **Scan for scales** to select it, or enter its Bluetooth address.
   Leave the adapter at `hci0` unless you use
   another adapter. Wait for **ready for the next grind** and compare the
   card's live weight with the scale's display and a known weight before
   using automatic reporting.
5. Use **Test shot** on its card to check measurement without grinding:
   place an empty cup first, wait for tare, pour water for at least five
   seconds, then stop and leave the cup until the result appears. The test
   cannot adjust the grinder. **Abort test** cancels only that test.

The card keeps showing live weight after each shot, alongside the last
measurement's time. During a shot, the brewing screen shows fresh weight
and time from first drops. You can switch to manual entry at any time.

| Setting | Flag | Environment | Default |
|---|---|---|---|
| Scale Bluetooth address | `--bookoo-address` | `BOOKOO_ADDRESS` | off |
| Mini Bluetooth address (optional) | `--bookoo-mini-address` | `BOOKOO_MINI_ADDRESS` | first-drop timing |
| Mini Bluetooth token | `--bookoo-mini-token` | `BOOKOO_MINI_TOKEN` | required with Mini address |
| Bluetooth adapter | `--bookoo-adapter` | `BOOKOO_ADAPTER` | `hci0` |

## Optional Mini paddle timing

See [Mini paddle setup](../bluetooth_scale/README.md) to tare at paddle start
and measure paddle start-to-stop, including pre-infusion. In that mode a
**Test shot** arms measurement and waits for the paddle; pouring alone does
not start it. The scale's own timer display is not controlled. The workflow
below describes the default, with no Mini address configured.

## Pull a shot

Keep the scale on and connected before grinding, and put the cup on it
before the grinder's shot request. Use a GbS recipe. For an E64 WS, press
the rotary knob when prompted, then start brewing; an E65/E80 GbS requests
the shot when its grind finishes. The integration tares the scale at that
request. Prepare the puck and start the espresso machine normally, stopping
it yourself at your desired yield.

The scale detector measures from the **first drops to flow stop**, including
drips. This differs from the Mini's pump-start timer: set the grinder's
target time consistently with this measurement. After the weight settles,
gbs-anywhere sends the result to the grinder. A cup lifted after the shot
also ends the measurement. Do not tare during extraction.

Keep one automatic integration active for this workflow. With both this and
La Marzocco cloud reporting, whichever submits first wins, and their time
definitions differ.

## Connection problems

The integration reconnects automatically. If it was not ready before the
grind, or loses readings during a shot, enter that shot manually. It does
not tare on reconnect or report a partial shot. Wait for it to reconnect
before the next grind. A scale switched off or attached to another app may
not be available. A Bluetooth adapter on your phone cannot substitute for
the adapter on the computer running gbs-anywhere.

The connection stays open while the scale is on. The integration changes
no beep, auto-off or timer settings; its only scale command is tare. The
scale's own timer is not used because it can stop and reset; measurements
use the host's monotonic arrival times. Bluetooth interruptions over two
seconds invalidate the measurement rather than being treated as flow stop.
