//! Minimal host binary for replaying the Grim Fandango intro cutscene.
//!
//! The crate intentionally keeps to the smallest viable surface area: it boots
//! the retail Lua bundle, simulates the intro playback locally, and exits.

use anyhow::Result;

use grim_engine::{parse_args, run_intro};

fn main() -> Result<()> {
    let args = parse_args();
    run_intro(args)
}
