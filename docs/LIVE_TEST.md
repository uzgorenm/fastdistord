# Live acceptance checklist

Not run in the cloud. Requires an explicitly opted-in user, locally entered credential, authorized existing guild/channel, microphone permission, Apple Silicon Mac and second participant. Do not run on an important account without understanding Discord's unofficial-access risks.

- Close Discord desktop and web entirely. Launch the local `.app`.
- Connect in session-only mode. Verify intended account and existing guild.
- Join only the intended authorized channel. Do not unmute until the second participant is ready.
- Confirm DAVE readiness; transport/UDP readiness alone must not start audio. Refuse unsupported/no-DAVE sessions.
- Transmit live microphone and receive the other participant. Verify two-way audio with headphones.
- Verify mute immediately silences actual remote audio, including speech queued before mute. Repeat while typing, hidden, and during DAVE member changes.
- PTT: press/release, focus change, hiding, lost focus, sleep, disabled shortcut. Uncertain state must fail muted; mute remains authoritative.
- Deafen suppresses receive and transmit; undeafen preserves prior explicit mute.
- Add/remove multiple participants, sustain call, inspect speaking state and mix/clipping/drop counters.
- Test administrator mute/deafen, move, kick and permission removal. Never auto-undo; no reconnection to a removed channel.
- Leave/rejoin/switch repeatedly. Race Leave/Logout against connecting and joining. No stale join may activate devices.
- Unplug/default-route switch/Bluetooth/sample-rate changes/mic denial: stop or recover explicitly, no capture after Leave.
- Drop Wi-Fi/reconnect, suspend/resume. No uncontrolled reconnect loops or stale transmit buffers.
- Close window for 10 minutes while calling; reopen from tray. Verify receive/transmit/PTT without rendering.
- Logout deletes saved Keychain entry if one was opted into; Quit releases devices and process exits.

Measure a **release build on the target Mac**: launch-to-window, resident memory, CPU as fraction of one core, underruns, queue drops, and output latency. States: disconnected, connected silent, active one/two/multiple participants, hidden active, and repeated reconnects. Provisional goals: idle RSS<100 MB, call RSS<150 MB, idle CPU<1% of one core. These are goals, not measured claims. Compare the official client only with equivalent actual channels/participants/settings on the same machine.
