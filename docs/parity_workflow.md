# Parity Divergence Workflow

Guidance for contributors working on retail/engine parity. The goal is to **recreate retail behaviour**, not to cosmetically align logs. Use this as a checklist before changing `grim_engine` or telemetry.

## Ground truth
- Retail telemetry is the source of truth. Do not suppress retail events or invent engine events to make traces line up.
- `grctl` parity logs default to the semantic stream; use `--raw` only when you need VM details to debug stack mechanics.
- Divergences are meaningful signals. If the engine emits an extra event, assumes a ref order, or omits a call, treat it as a real behavioural gap to fix.
- The boot window ends once `_system.lua`/`BOOT(false)` completes. Engine runs now stop there; post-boot script driving and cutscene plumbing are stripped for parity captures.

## Workflow (per bug/feature)
- Start with a short run to find the first break: `./grctl.sh parity start --timeout 3` then `./grctl.sh parity logs` (or `--from-start`) to spot the earliest divergence.
- Work on the **first** divergence you see. Ignore later “end-of-script” errors until the earlier mismatch is resolved; they often disappear once the first gap is fixed.
- Confirm whether the divergence is semantic (wrong/missing binding, ref order, table shape) or mechanical (stack push order, GC timing). Fix semantics first.
- Re-run the same short window after each change. Only extend the timeout when the early window is clean and you need to chase later behaviour.
- Stop once the earliest divergence is resolved. Do not keep coding through the next failure in the same PR; re-run to surface the new earliest diff, then open a fresh change for that.

## Do / Don’t
- Do implement the missing behaviour (e.g. register the real binding, call the real fallback, create/populate tables in the observed order).
- Do add targeted telemetry if you need more detail, but keep the semantic/raw split intact.
- Do keep the engine focused on the boot window for parity work; defer runtime loops, movie playback, or stubbed script drivers to later passes.
- Don’t mask or reorder events just to silence the diff; if retail sets a tag method, the engine must set the same hook for the same reason.
- Don’t gate retail-consistent code paths behind debug flags to “pass parity”; parity should fall out of correct behaviour.

## Example: first divergence fix
- Context: earliest mismatch was in bootstrap ordering. The engine bound control stubs before `_TRIGMODE` and stored `system` under a new ref instead of retail’s ref 0, causing downstream ref/order skew.
- Fix: remove the early control stub bindings, bind `_TRIGMODE` directly after legacy IO/errorfb, and store `system` with `ref=0` before populating `controls` via `lua_getref`. After this, the semantic streams matched through controls setup.
- Result: the next earliest divergence surfaced deeper (_actors.lua expecting Actor methods), showing the workflow: clear the earliest diff, rerun the short window, let the next real gap emerge.

## Telemetry surfaces
- Engine instrumentation lives in `grim_engine/src/lua_host/telemetry.rs`; prefer its shared helpers (`caller_origin_fields`, `origin_fields_for_ptr`) over ad-hoc origin handling so semantic/raw streams stay comparable to retail.
- Retail shim instrumentation lives in `grim_analysis/src/trace`; it uses the same helpers and symbol map fallback for unresolved symbols.
- Shared telemetry utilities live in `grim_telemetry_schema/src/trace_utils.rs`; use `is_runtime_frame` as the baseline skip list before adding crate-specific skips to keep caller attribution consistent.
- Handle formatting and preview length helpers are centralized in `grim_telemetry_schema::trace_utils` (`handle_hex`, `LOG_PREVIEW_MAX_LEN`) so tweaks stay in sync across the engine and retail shim.

## When adding fixes
- Keep changes tightly scoped to the observed divergence; avoid opportunistic refactors in the same PR so parity work stays reviewable.
- Keep each PR scoped to a single resolved divergence. If the rerun reveals a new earliest mismatch, pause and document it for the next fix instead of folding it into the current work.
- Capture the before/after divergence (log snippet or note) in the PR description so others understand what was fixed.
- If you need to stub functionality, do so in a way that still mirrors retail sequencing and surface shape. Leave TODOs that reference the retail behaviour to be implemented.
