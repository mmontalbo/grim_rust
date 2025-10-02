# Grim Fandango Modding Playground

Local workspace for iterating on Grim Fandango Remastered data files. The repo
already contains decompiled Lua scripts and other assets extracted from the
retail LAB archives.

## Prerequisites
- Linux system capable of running 32-bit binaries. Install Steam's
  `steam-run` runtime (ships with the Steam client on NixOS/Flatpak/other distros)
  so the helper script can wrap the original executable with the required
  libraries.
- Nix (optional but recommended) to enter the provided shell environment with
  the ScummVM toolchain, `gdb`, and helper utilities.

## Environment

Enter the toolchain shell whenever you need the `grim_unlab`/`grim_luac`
utilities:

```bash
nix-shell
```

The shell exports `GRIM_INSTALL_PATH`, pointing at your Steam installation. You
can override it before launching `nix-shell` if your install lives elsewhere.

## Local Dev Install Workflow

Use `grim_mod` to manage a writable copy of the game inside the repo:

```bash
./grim_mod status          # show configured source + dev paths
./grim_mod ensure-install  # rsync from $GRIM_INSTALL_PATH into dev-install/
./grim_mod launch          # run the dev copy with debug logging enabled
./grim_mod debug           # drop into gdb with the dev copy preloaded
```

`launch` runs the Linux executable with `--classic --debuglevel 1` and writes a
log file to `dev-install/grim_dev.log`. Pass additional flags after
`--` (e.g. `./grim_mod launch -- --debuglevel 5`).

### Debugging

`grim_mod debug` starts GDB with the same runtime setup (always wrapped by
`steam-run`, with `LD_LIBRARY_PATH` pointing at the dev install, and your usual
default flags). Run it inside the provided `nix-shell` to ensure `gdb` is
available, or install a 32-bit capable GDB on the host. Example:

```bash
nix-shell --run './grim_mod debug'
```

Add extra game flags after `--` and GDB options before it (e.g.
`./grim_mod debug -- --debuglevel 5`).

## Next Steps
- Rebuild a trivial Lua tweak with `grim_luac` and package it into a
  `DATA000PATCH*.LAB` file.
- Drop the new patch into `dev-install/` (or teach `grim_mod` a new
  subcommand to stage it) and validate the change in-game.
- Document the full rebuild + test loop in `wip.md` as workflows stabilize.

See `wip.md` for the current investigation log and immediate todos.
