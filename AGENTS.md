# AGENTS.md

Instructions for coding agents working in this repository.

## Project Shape

- `battery-up` is a Linux/NixOS Rust workspace for tracking notebook time spent running only on battery power.
- The default package and app are the CLI only; the COSMIC applet is intentionally a separate package because it pulls a heavier UI dependency stack.
- Power state is read from `/sys/class/power_supply`.
- The daemon/status state file defaults to `/var/lib/battery-up/state`; use a temporary `--state-file` for local tests.

## Repository Map

- `crates/battery-up-core`: shared power-supply reading, state parsing/writing, history, and unit tests.
- `crates/battery-up-cli`: terminal UI, daemon/status/reset commands, argument parsing, JSON output.
- `crates/battery-up-cosmic-applet`: COSMIC applet that reads daemon state and displays the accumulated time.
- `flake.nix`: packages, apps, overlay, dev shell, and NixOS module.
- `data/`: desktop entry and symbolic icon for the COSMIC applet.

## Development Commands

Prefer the Nix shell when dependencies are needed:

```sh
nix develop path:$PWD
```

Inside the shell:

```sh
cargo fmt --all
cargo test
cargo run -- --once
```

Useful targeted checks:

```sh
cargo test -p battery-up-core
cargo test -p battery-up
cargo build -p battery-up --profile release_cli
cargo build -p battery-up-cosmic-applet --profile release_applet
nix build path:$PWD#cli
nix build path:$PWD#applet
nix flake check path:$PWD
```

## Validation Guidance

- For core state, sysfs parsing, or serialization changes, run `cargo test -p battery-up-core`.
- For CLI behavior, JSON output, argument parsing, or daemon/status/reset changes, run `cargo test -p battery-up` when tests exist and at least one relevant `cargo run -- ...` smoke test.
- For Nix packaging, overlays, app definitions, or the NixOS module, run the smallest relevant `nix build path:$PWD#...`; use `nix flake check path:$PWD` for broader changes.
- For applet or desktop/icon changes, build `path:$PWD#applet` if the environment has the COSMIC dependencies available.
- Avoid running heavyweight applet builds for unrelated CLI/core-only edits.

## Coding Conventions

- Keep the workspace split intact: reusable logic belongs in `battery-up-core`; command/UI formatting belongs in the CLI; COSMIC-specific code stays in the applet crate.
- Preserve the current state-file format unless the task explicitly requires a migration. Existing fields should remain backward compatible where practical.
- Prefer standard-library Rust and existing dependencies; add dependencies only when they remove clear complexity.
- Keep output formats stable, especially JSON field names documented in `README.md`.
- Use explicit, saturating arithmetic for elapsed time and counters where overflow or clock jumps are plausible.
- Use temporary paths for local daemon/status tests, for example `--state-file /tmp/battery-up-state`.
- Do not edit generated build outputs such as `target/`, `result`, or `dist/`.

## Documentation

- Update `README.md` when commands, user-visible behavior, JSON fields, Nix usage, or systemd/NixOS module options change.
- Keep examples aligned with `nix run path:$PWD` because this repository is commonly used from a local checkout.

## Release Notes

- Version is currently duplicated in `Cargo.toml`, `flake.nix`, and README examples. If changing one for a release, check the others.
