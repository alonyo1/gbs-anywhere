# Linea Mini paddle timing with a Bluetooth scale

This optional mode works with the BOOKOO Themis Mini driver on Linux. It
needs a Mini R with the live Bluetooth shot-counter characteristic. The
implementation checks the actual service; model year or
an assumed firmware version alone does not establish support. This feature
has protocol, simulated Bluetooth and workflow tests, but has not yet been
verified with a physical Mini R and scale.

## Set up

1. Set up and validate the scale first. Use manual weighing mode and grams.
   Compare the app's live weight with the scale and a known weight.
2. Close other apps connected to the Mini. In the scale's **Edit** form,
   **Scan for the Mini** lets you select its address separately from the
   scale address. Scanning only fills the chosen field; save to connect.
   Both devices use the configured Linux adapter, normally `hci0`.
3. Obtain a local Bluetooth token. Put the Mini in Bluetooth pairing mode:
   power it off, move the paddle left, power it on, wait 10 seconds, then
   move the paddle right. See
   [La Marzocco’s pairing instructions](https://support-iot.lamarzocco.com/home-machines/).
   On the Linux host, run the following command with the Mini's address.
   With this Docker build already running:

   ```sh
   docker exec gbs-anywhere /usr/local/bin/gbs-anywhere \
     --read-mini-token AA:BB:CC:DD:EE:FF
   ```

   For a native installation:

   ```sh
   gbs-anywhere --read-mini-token AA:BB:CC:DD:EE:FF
   ```

   Add `--mini-token-adapter hci1` if needed. This command reads the token
   directly over Bluetooth, prints it to your terminal and exits. It starts
   no server and makes no cloud request. Treat its output as a secret.
   If the Mini cannot be reached, check pairing mode, Bluetooth range and
   whether another app is holding its connection.
4. Paste the token into **Linea Mini Bluetooth token** in the scale form,
   then save. A token obtained through your own existing La Marzocco setup
   also works. The token is saved locally in the private settings file, like
   other integration secrets; the app API never sends it back. Neither your
   email nor account password is needed for this mode.
5. Wait for the scale integration to show ready before requesting a shot.
   Missing firmware support or authentication failure appears on its card.
   Keep only one automatic shot reporter enabled to avoid competing results.

Leave **Linea Mini Bluetooth address** empty to disable paddle timing and
return to the original first-drops workflow. The existing saved token can
remain hidden for later reuse. Do not configure two scales against the same
Mini simultaneously: each configured scale holds its own Mini connection.

## Pull a shot

1. Grind using a GbS recipe. On E64 WS, press the knob when prompted; on
   E65/E80 GbS, completion of the GbS grind arms the shot.
2. Prepare the puck and put the empty cup on the scale. The app shows that
   it is waiting for the Mini paddle. The scale has not tared yet.
3. Move the paddle to brew. A fresh Mini brew-start notification tares the scale
   and starts the **app's timer**, including time before the first drops.
4. Stop the Mini yourself at the desired yield. The time freezes at brew
   stop. Leave the cup on the scale until its weight is steady for about
   three seconds; late drips count toward yield, not shot time.
5. The measured time and final weight are reported to the grinder. Use a
   grinder target time consistent with paddle start-to-stop timing.

**Test shot** arms only the selected scale without adjusting the grinder.
In this mode it also waits for the Mini paddle, then follows the same cycle.
A plain pour without operating the Mini does not start the test. Abort the
test from the card if needed.

This does not start/stop the Mini pump or drive the scale's physical timer
display. It only reads Mini notifications and sends tare to the scale.

## Incomplete or interrupted measurements

- Subscription replay, telemetry, duplicate starts and mid-shot timer
  updates cannot trigger tare. A connection established after a grinder
  request leaves that request for manual reporting.
- Starts older than the armed request or received too late are not used.
  Tare must be sent promptly and zero confirmed within 1.5 seconds of the
  start event. If coffee reaches the cup too soon for this, use manual entry.
- Pre-infusion pauses cannot finish the shot. Both a Mini stop event and a
  stable final weight are required. Shots under five seconds or five grams
  are not reported; the weight must also reach half the recipe target when
  available. A missing stop event leaves the shot for manual entry or the
  configured reporting deadline.
- Flushes and backflushes are discarded when the Mini identifies them. A
  flush may look like a brew at its start: perform cleaning/rinsing before
  arming GbS, not between arming and extracting.
- A scale or Mini disconnection invalidates the active measurement. It
  never resumes or tares partway through a shot. Enter the result manually
  and wait for ready before the next grind. Removing the cup before the
  final weight settles also requires manual entry.
- The Mini may accept only one central Bluetooth connection. Close the
  La Marzocco app and other clients while using this feature. The adapter
  must support simultaneous connections to the scale and Mini.

The Bluetooth UUIDs, authentication acknowledgement, JSON event fields and
subscription replay behavior are documented by the MIT-licensed
[pylamarzocco implementation](https://github.com/zweckj/pylamarzocco/blob/main/src/pylamarzocco/clients/_bluetooth.py)
and its [notification models](https://github.com/zweckj/pylamarzocco/blob/main/src/pylamarzocco/models/_bluetooth.py).
The Rust implementation uses the existing BlueZ D-Bus transport and adds no
runtime dependency or vendor cloud connection.
