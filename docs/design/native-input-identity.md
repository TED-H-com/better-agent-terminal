# Ordinary native input identity

Authenticated existing session metadata adds `inputState`:
`{schema:"bat_input_state_v1",epoch,received,pending}`. The epoch is a random
process-local runtime incarnation. `received` counts handler input entries
before preparation or queue/turn-lock waits; `pending` includes those waits and
unsettled processing. Counters never derive from SDK usage `numTurns`, transcript
length, idle, ACK, or filenames. Errors and cancellations retain the received
count; settled work decreases pending once. Duplicate Claude client-message IDs
retain the original entry. A replaced/reloaded runtime changes epoch, and losing
Codex native-thread ownership invalidates the old session's epoch.

Rust registers each entry while holding the session identity lock, then verifies
that same counter identity after taking the turn-operation lock. Reset/reload
cannot move an old queued send or context injection into the replacement runtime.
Dropping an old entry decrements only its original epoch. Claude's queued callback
likewise checks its captured runtime generation before reaching the SDK; reload
invalidates old callbacks and their finalizers retain the old counter object.

The Rust Codex producer counts ordinary send and context injection. Its pending
count covers each local request; existing `isStreaming` continues to cover the
native turn after ACK. Claude retains pending across the complete queued native
turn. Consumers require both zero pending and inactive native metadata alongside
an independently captured, completed original native turn. Missing identity from
other runtime adapters remains unknown. This field grants no write authority and
changes no command, replay event, default confinement, or dedicated PM proof gate.

Connector may bind an original first send only from an authenticated fresh
snapshot with this epoch at zero received and pending. Only that same epoch with
one received input, zero pending, exact SDK/registry/source identity, and the
original completed turn can remove a maintenance workload blocker. Old receipts
cannot gain this binding retroactively.
