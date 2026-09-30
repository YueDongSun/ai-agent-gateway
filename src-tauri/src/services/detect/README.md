# `src-tauri/src/services/detect/` — External-app coexistence detection

Detects whether another local app is writing the same live agent config
files AAG writes. Today this is just cc-switch, but the public surface is
grouped here so future detectors (other proxy managers, alternate CLI
launchers, etc.) can land alongside it.

## Anchor files

- [`mod.rs`](mod.rs) — module entry point. Exposes the public detector
  surface so future detectors slot in next to `cc_switch`.
- [`cc_switch.rs`](cc_switch.rs) — `cc_switch` coexistence detector.
  Combines four orthogonal signals (TCP probe of cc-switch's default
  port `15721`, file-system probes for cc-switch artefacts in
  `~/.cc-switch/`, etc.) into a `CcSwitchStatus`. Exposes
  `CC_SWITCH_DEFAULT_PORT` so the frontend can render the port without
  hard-coding the magic number.

## Key contracts

- Detection is best-effort and cheap (one TCP probe plus a few file
  probes), so it is safe to call on every UI mount and on every provider
  switch.
- A detected cc-switch conflict can be overridden by the user via the
  `ccswitch_override_enabled` setting (advanced users only).
- The strict-write block is gated behind `ccswitch_block_enabled`. The
  banner is always shown; the toggle only flips the hard-block behavior.

## See also

- [`../provider/`](../provider/README.md) — the live writers that check
  this detector before writing.
- [`../import_cc_switch/`](../import_cc_switch/README.md) — the explicit
  one-shot import path that complements detection.
- [Root README](../../README.md).