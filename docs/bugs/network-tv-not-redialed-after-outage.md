# A network TV that stays on through an outage is never reconnected

**Status:** open; fixed on `fix/network-link-recovery`, pending the checks on the TV below. Opened 2026-11-15, found on the packaged 0.6.0 stack after a Wi-Fi router's firmware reboot.

**Severity:** high. The TV stays lit and unmanaged until someone notices: every screen-off sent to it is acked and dropped, so an OLED runs its own screen saver over the PC's picture for as long as the daemon lives.

## Symptom

The TV shows its own screen saver over the PC's input while the desk is idle or streaming. `tisvlay list` shows it disconnected, with nothing reconnecting it, though it answers on the network. Idle, lock and stream actions log as sent, and nothing reaches the TV.

What happened, reconstructed from the timestamps of the daemon's zenoh samples (the journal of that night was gone) and read-only SSAP probes of the TV:

| When | What |
|---|---|
| day 1, 11:07 | The TV's last power report: on, screen off. Later screen-offs changed nothing, so nothing was republished. The session stayed idle from 11:11 on. |
| day 2, 01:02:39 | During the router's reboot the TV's link went down. No dial followed: no `connect-failed`, never `reconnecting`. |
| 01:06:54 | The network was back. |
| 03:14:20 | A stream ended and the session sent a screen-off. The daemon acked it as satisfied without sending anything. |
| 12:46 | Probe: the TV's power state is `Screen Saver`, the PC's input in front with a signal. Five more screen-offs over the next minutes were acked and dropped the same way. |

The PC's own network never changed (no carrier transition, and the route to the TV stayed in place), so the carrier and route watchers had nothing to report.

## Cause

Two rules that were right for a TV going to standby and wrong for one behind an outage, and a third that hid the result:

- A network TV's dropped link was never redialed, on the reasoning that the user had turned the TV off. Only a carrier or route change on the PC redialed a parked TV, and a router rebooting elsewhere on the network is neither.
- A screen-off to a disconnected TV was acked as already dark.
- The session re-blanks a panel that lights itself while idle, but it read the panel from the last power report alone, so a TV it could no longer see kept its last state forever.

A network TV announces a standby before it closes its link (its power report goes to an off state first), and a power-off of the daemon's own closes the link from the daemon's side. A drop with neither is the network's, with the TV as on as it was.

## Fix

On the branch: a drop the TV didn't announce marks it unreachable and redials it until it answers; a TV that never answered a whole dial budget is redialed the same way, and one that answers and turns the dial down is not; nothing is held for an unreachable TV, whose screen-off and power-off are refused instead of acked; and the session counts a panel as lit only while its TV is connected, so one that comes back lit is re-blanked after the profile's idle timeout. `docs/architecture.md` § The world has the rules.

## Checking it on the TV

The TV is the test PC's only display and these passes blank it, so each runs as one unattended block: read the block's first lines for what to do and when, start it, and touch nothing but the TV's remote where it says so, until `DONE` prints. Pass A takes about 7 minutes and pass B about 19.

Each pass cuts the TV off from the PC the way that night did, without touching the router: an nftables table of its own rejects the PC's traffic to the TV as unreachable, and the daemon's open connection is killed (`ss -K`, which needs a kernel with `CONFIG_INET_DIAG_DESTROY`) so the drop doesn't wait on a send. The table is named, so a run that died is undone with `sudo nft delete table inet tisvlay_outage`.

Before a pass: the stack under test is running and the TV connected, the desk is unlocked, and nothing is streaming. Set the variables for the TV and the stack; `nft` is spelled out because root's path may not have it (on NixOS, `nix build --no-link --print-out-paths nixpkgs#nftables` gives the directory).

```fish
set tv_ip 192.0.2.10                                 # the TV's address
set tv_id 00000000-0000-0000-0000-000000000000       # its uuid, from `tisvlay list`
set idle 20                                          # the test profile's idle timeout, in seconds
set nft /usr/sbin/nft                                # the nftables binary
```

The passes wait out the idle timeout twice, so they run under a user profile of their own with a short one, assigned to the TV for the session under test and removed afterwards. Once per stack, before its passes:

```fish
set was (tisvlay user-profile $tv_id)
set check (tisvlay profile user create outage-check --body "{\"unlocked_idle_timeout_seconds\": $idle, \"locked_idle_timeout_seconds\": $idle}")
tisvlay user-profile $tv_id $check
tisvlay user-profile $tv_id --effective
```

And after them, which also undoes it for a run that died:

```fish
tisvlay user-profile $tv_id $was
tisvlay profile user delete $check
```

### Pass A: the outage, and the return

Run it on 0.6.0 to reproduce, then on the branch. On the branch the link returns on its own and the TV, found lit or not, ends dark.

```fish
sudo -v
begin
    fish -c 'while true; sudo -n -v; sleep 50; end' &
    set keepalive $last_pid
    set start (date '+%F %T')
    echo "$start: hands off the keyboard, mouse and TV until DONE, about "(math "($idle * 2 + 420) / 60")" min"
    tisvlay user-profile $tv_id --effective
    function snap; date +%T; tisvlay --json list | jq -c --arg id $tv_id '.[$id] | {link: ."state/link", power: ."state/power"}'; end
    fish -c 'tisvlay watch | while read -l line; echo (date +%T) $line; end' &
    set watcher $last_pid
    echo "waiting $idle s for the idle timeout to blank the TV"; sleep (math $idle + 30); snap
    echo "outage starts"
    sudo $nft add table inet tisvlay_outage
    sudo $nft add chain inet tisvlay_outage out '{ type filter hook output priority 0; }'
    sudo $nft add rule inet tisvlay_outage out ip daddr $tv_ip reject with icmp type net-unreachable
    sudo ss -K dst $tv_ip
    sleep 20; snap
    sleep 160; snap
    echo "outage ends"
    sudo $nft delete table inet tisvlay_outage
    sleep 90; snap
    echo "waiting $idle s for the idle timeout to re-blank a TV found lit"; sleep (math $idle + 30); snap
    pkill -P $watcher; pkill -P $keepalive; kill $watcher $keepalive
    journalctl -t tisvlay-daemon -t tisvlay-session --since "$start" --no-pager -o short-precise | grep -v 'qualifies for socket access'
    echo DONE
end 2>&1 | tee ~/tisvlay-outage-a.log
```

**Expect on 0.6.0:** the link goes down at the outage and is still down at the end, with no dial in the journal.

**Expect on the branch:** the journal says the connection dropped with no standby announced and is being redialed; the link is back within the 90 s after the outage ends; and the last snapshot reads screen off, with the session's journal showing the idle action going out again if the TV came back lit. Whether the TV came back lit is the answer to whether it leaves its screen-off state by itself when its client goes away.

### Pass B: the TV turned off during the outage

On the branch only. It checks that redialing a TV that is off neither wakes it nor keeps it awake, which the "redial until it answers" rule rests on. Start a timer when you start the block: the remote is needed twice, at times the block prints first.

```fish
sudo -v
begin
    fish -c 'while true; sudo -n -v; sleep 50; end' &
    set keepalive $last_pid
    set start (date '+%F %T')
    set off_at (math $idle + 120)
    set on_at (math $idle + 900)
    echo "$start: hands off until DONE, about "(math "($idle + 1080) / 60")" min, except:"
    echo "  at "(math "$off_at / 60")" min: turn the TV OFF with its remote"
    echo "  at "(math "$on_at / 60")" min: turn the TV ON with its remote"
    function snap; date +%T; tisvlay --json list | jq -c 'map_values({name: ."meta/identity".name, link: ."state/link", power: ."state/power"})'; end
    fish -c 'tisvlay watch | while read -l line; echo (date +%T) $line; end' &
    set watcher $last_pid
    sleep (math $idle + 30); snap
    echo "outage starts"
    sudo $nft add table inet tisvlay_outage
    sudo $nft add chain inet tisvlay_outage out '{ type filter hook output priority 0; }'
    sudo $nft add rule inet tisvlay_outage out ip daddr $tv_ip reject with icmp type net-unreachable
    sudo ss -K dst $tv_ip
    sleep 180; snap
    echo "outage ends, with the TV off since about a minute after it started"
    sudo $nft delete table inet tisvlay_outage
    for minute in (seq 10)
        sleep 60; snap; ping -c1 -W1 $tv_ip >/dev/null; and echo "the TV answers ping"; or echo "the TV doesn't answer ping"
    end
    echo "the TV should be turned on about now"
    sleep 150; snap
    pkill -P $watcher; pkill -P $keepalive; kill $watcher $keepalive
    journalctl -t tisvlay-daemon -t tisvlay-session --since "$start" --no-pager -o short-precise | grep -v 'qualifies for socket access'
    echo DONE
end 2>&1 | tee ~/tisvlay-outage-b.log
```

**Expect:** through the ten minutes the TV is off, its link stays down, its CEC twin (if one is configured) keeps reporting standby, and the TV itself stays dark, with no pairing prompt and no wake. Whether it keeps answering ping says whether the dials hold it out of deep standby. Turned on again, it reconnects within about a minute without being asked.

If the TV wakes, prompts or is kept from deep standby, the rule that an off TV may be redialed is wrong for it and is revisited before the branch merges.

## Results

**Pass A on 0.6.0 (2026-11-15, 19:59, LG C3).** Reproduced. The idle timeout blanked the TV 20 s in (`state/power` to screen off at 19:59:21). The outage began at 19:59:50; one second later the daemon logged `WebSocket read error, reader task exiting` and `state/link` went to disconnected, so the killed socket is noticed at once and doesn't wait on a send. The link was still down, never `reconnecting`, 20 s into the outage, at its end three minutes later, 90 s after the path was back, and at the end of the pass; the daemon's journal has nothing after the read error. The last power report stayed "on, screen off" throughout, the stale state the session used to trust.

The block's background helpers kept its output pipe open for up to 50 s after `DONE`, which delayed the profile's restore that long; the blocks now end their helpers' children first.
