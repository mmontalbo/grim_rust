# Live Stream Pipeline Vision

This document captures the target architecture for live, frame-by-frame
inspection of Manny's office by pairing the retail executable with the Rust
host. The goal is to let `grim_viewer` render the retail framebuffer next to
our simulation overlays while both feeds advance in lockstep.

## Existing Offline Flow
- **Retail executable** launches via `tools/run_dev_install.sh` and writes
  `mods/telemetry_*.json*` plus `telemetry.log` after the run completes.
- **Rust host (`grim_engine`)** replays the boot sequence and emits JSON
  snapshots on demand (`--timeline-json`, `--movement-log-json`, etc.).
- **Viewer (`grim_viewer`)** consumes the static JSON files alongside decoded
  plate assets and renders overlays in a single viewport.

This batch model is perfect for reproducible analysis but fails the “watch the
simulation evolve live” milestone.

## Live Capture Goals
1. **Low-latency retail frames** — grab the rendered framebuffer every frame
   (or at a capped refresh rate), timestamp it, and send it over a socket.
2. **Streaming Rust state** — surface Manny’s position, hook coverage, audio
   cues, and hotspot changes from `grim_engine` as they happen instead of as a
   post-run dump.
3. **Synchronised viewer** — teach `grim_viewer` to subscribe to both feeds,
   render the retail video in one pane, our simulation in the other, and expose
   timeline controls (pause/step/overlay diff).

## Constraints & Assumptions
- We keep everything runnable offline on the dev workstation; no hosted
  services or cloud orchestration.
- Retail and host streams originate on the same machine most of the time, so a
  local TCP socket (or Unix domain socket) is acceptable. Remote viewing should
  remain possible with minimal changes.
- Initial implementation prioritises correctness and debuggability over raw
  throughput. We can add compression or GPU sharing later if capture bandwidth
  becomes a bottleneck.
- The viewer must gracefully handle gaps (retail pause menus, host stalls,
  varying framerates) and surface them in the UI.

## Protocol Overview

We introduce a shared framing protocol called **GrimStream v1**. Each message
travels over a persistent, ordered byte stream (TCP/UDS) and uses a fixed header
followed by a payload encoded as MessagePack. The header keeps binary payloads
compact while letting us route by message kind without parsing the entire body.

```
struct GrimStreamHeader {
    magic: [u8; 4];   // "GRIM"
    version: u16;     // 0x0001
    kind: u16;        // enum MessageKind
    length: u32;      // payload bytes
}
```

### Message Kinds

| Kind | Producer | Purpose |
|------|----------|---------|
| `HELLO` | All | Sent immediately after connect. Payload: `{ protocol: "GrimStream", producer: "retail" | "engine" | "viewer", build: "..." }`. |
| `STREAM_CONFIG` | Retail | Describes the framebuffer stream: `{ width, height, pixel_format, stride, nominal_fps }`. |
| `FRAME` | Retail | Raw RGB frame data. Payload: `{ frame_id, host_time_ns, telemetry_time_ns?, stride_override?, data: [u8; ...] }`. Data follows the MessagePack map as a binary blob (no base64). |
| `TELEMETRY` | Retail | Incremental telemetry emitted by the Lua shim. Mirrors the JSONL schema: `{ seq, label, data, timestamp }`. |
| `STATE_UPDATE` | Engine | High-frequency Manny state deltas: `{ frame, position, yaw, active_hotspot?, coverage_delta?, audio_cues? }`. |
| `TIMELINE_MARK` | Engine | Emits discrete hook invocations tied to the movement scrubber. |
| `CONTROL` | Viewer | Optional back-pressure commands (pause, step, seek). |
| `HEARTBEAT` | All | Keeps idle connections alive; includes `host_time_ns`. |

### Time Base
- All timestamps use nanoseconds from `CLOCK_MONOTONIC`.
- Each producer nominates its first `FRAME` or `STATE_UPDATE` as `t0`. The
  viewer keeps per-stream clock offsets and aligns them when rendering.
- When the retail telemetry shim provides an in-game tick, we forward it in
  `telemetry_time_ns` so the viewer can compare sim time vs. wall clock.

### Error Handling
- If the viewer encounters an unknown message kind, it logs and ignores it,
  keeping the connection alive.
- Producers resend `STREAM_CONFIG` after reconnects or when the format changes
  (e.g., resolution toggle).
- Messages never span connections; reconnects are free to reset `frame_id`.

## Retail Capture Pipeline

We will introduce a new Rust binary `tools/live_retail_capture.rs` that:
1. Launches `tools/run_dev_install.sh` (optionally through `steam-run`) and
   captures the game window using `ffmpeg` in `x11grab` mode.
2. Pipes raw RGBA frames from `ffmpeg` (`-f rawvideo -pix_fmt rgba`) into the
   Rust process.
3. Attaches to the existing telemetry files (`telemetry_events.jsonl`) via
   incremental reads and forwards entries as `TELEMETRY` messages with the same
   sequence numbers the offline analysis expects.
4. Forwards frames and telemetry over a single GrimStream connection to any
   subscriber (initially `grim_viewer`).
5. Writes optional archival artifacts (PNG dumps, JSON derivatives) behind
   flags so automation can still persist runs without blocking the live stream.

Key implementation points:
- Use `tokio` for async IO; spawn an `ffmpeg` child process, wrap its stdout in
  an async reader, and re-chunk frames based on the negotiated resolution.
- Watch the telemetry JSONL file via non-blocking reads (no inotify dependency)
  and coalesce multiple events into a single `TELEMETRY` message when under
  load.
- Expose CLI flags for capture source (`--x11`, `--wayland`, future DXGI),
  target host/port, and optional frame downscaling.

## Rust Host Streaming

`grim_engine` already exposes Manny’s position, coverage counters, and hotspot
events through `EngineRunSummary`. For live streaming we will:
- Add a `--stream` flag to `grim_engine --run-lua` that binds a GrimStream
  server socket.
- After each scheduler tick (or movement/hotspot mutation), package the delta
  into a `STATE_UPDATE` message and send it over the socket.
- Reuse the existing coverage normalization so the viewer receives the same
  hook identifiers as offline diffs.
- Allow simultaneous use with the JSON output flags so scripted captures remain
  deterministic.

This change will live in a new module `grim_engine::stream` that owns the
socket lifecycle and serialization helpers.

## Viewer Integration Outlook

Once both feeds publish GrimStream messages, `grim_viewer` can:
- Establish two client connections (retail + engine) from the CLI.
- Maintain ring buffers for incoming frames and state updates, aligning them by
  timestamp.
- Render the retail frames in a new left-hand viewport while keeping the
  existing overlays in the right-hand pane.
- Provide timeline controls that apply back-pressure by sending `CONTROL`
  messages (initially no-op until producers implement pause/step).
- Surface telemetry warnings (dropped frames, out-of-sync clocks) in the HUD.

Implementation details for the viewer will be captured in a follow-up design
once the producer side is prototyped.

## Next Steps
1. Scaffold the GrimStream protocol crate (shared types, MessagePack helpers).
2. Build the retail capture binary with ffmpeg piping and telemetry tailing.
3. Add live streaming hooks to `grim_engine`.
4. Extend `grim_viewer` with the dual-pane UI and synchronization logic.

Keeping the protocol small and self-contained inside the repo lets us evolve it
quickly, test it with integration fixtures, and keep the Manny office milestone
moving without introducing external dependencies.
