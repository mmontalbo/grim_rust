# Work in Progress

## Current Context
- Repo mirrors decompiled assets extracted from `DATA000.LAB` and `IMAGES.LAB` using the `grim_unlab` toolchain available in the `nix-shell`.
- Local dev install staged at `dev-install/`; manage it with `./grim_mod` (e.g. `./grim_mod launch` starts the game with debug logging to `dev-install/grim_dev.log`).
- Short-term priority is setting up a smooth local development loop: staging a writable game install alongside the extracted sources so patches can be compiled, dropped in, and tested rapidly from the start of the game.

## Active Threads
- Prep local dev install: create a self-contained copy of the Grim Fandango Remastered data inside the repo that the executable can run against without touching the Steam/GOG files.
- Define patch injection workflow: confirm which LAB/patch files the dev build reads first and how to override them with locally rebuilt assets.
- Establish run/test routine: document how to launch the game in debug mode (including GDB), load the opening scenes, and validate that file changes propagate immediately.

## Next Steps
1. Ensure `steam-run` is available, then use `./grim_mod launch` to confirm the copied install boots and generates `grim_dev.log` with the default debug flags.
2. Identify which patch LAB overrides are loaded first and script a minimal rebuild flow for injecting Lua tweaks.
3. Rebuild a trivial Lua change (e.g., startup `PrintDebug`) into `DATA000PATCH*.LAB`, deploy it via `./grim_mod ensure-install`/manual copy, and confirm the change appears in-game.
4. Lock in debugger workflow notes (`./grim_mod debug`) and capture the full loop (commands, file paths, verification steps) in `wip.md` once verified.
