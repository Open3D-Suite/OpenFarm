# OpenFarm

A render farm for your own computers: scenes sent from 3ds Max or Blender render on every machine of a
local network. One Windows program in Rust: the farm's server, the agent that renders and the window.

This file holds facts about this repo only, each one checkable in the code. Update it in the same commit
as a change that makes it old.

## Layout

- A Cargo workspace: the app at the root and `crates/core` (pure logic, no input or output).
- `src/server/`: `mod.rs` the routes under `/api/v1`, `farm.rs` every rule of the farm, `db.rs` SQLite.
- `src/agent/`: `runner.rs` runs a task; `hosts.rs` keeps 3ds Max or Blender open between tasks.
- `src/local.rs`: the local app on 127.0.0.1 for the window (`src/window.rs`) and the submitters; it
  relays `/api/v1` to the server. `src/config.rs`: `config.json` and the ports (server 47100, local 47102).
- `ui/` (plain HTML, JavaScript and CSS, no build step; English and Spanish texts in `ui/app.js`),
  `hosts/*.py` (run inside 3ds Max and Blender) and `integrations/` (the submitters) are built into the exe.
- `tools/package.py`: the packager. `tests/*.py`: checks against a running farm, outside `cargo test`.

## Build and check

- Build: `cargo build --release --locked`.
- Checks, all of which must pass:
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`
  - `cargo test --workspace --locked` (77 tests; without `--workspace` the 18 of `crates/core` do not run)
  - `ruff format --check .` and `ruff check .`, for the Python files
- Clippy fails today: `main` is over the `cognitive_complexity` limit of 30.
- `.editorconfig`, `.gitattributes`, `rustfmt.toml`, `clippy.toml` and `ruff.toml` are the same in every
  Open3D repo. Do not change them here.

## Version and release

- The app's version is in the root `Cargo.toml` and nowhere else by hand, except the `**Version X.Y.Z**`
  line of the README and the CHANGELOG. The packager stops if that line or a CHANGELOG section is missing.
- Changes a user can notice go into `CHANGELOG.md` under `## In development`, in the same commit.
- `python -I tools/package.py` checks the licences of the dependencies, runs the tests, builds, and writes
  `dist/openfarm-<version>-windows-x64.zip` and `dist/SHA256SUMS.txt`. It refuses an exe that still holds a
  path of the computer that built it; its own flags keep them out (`/d1trimfile` for the bundled C code).
- The Open3D launcher needs `OpenFarm.exe` at the root of the zip, its file version that of the release.

## Rules of this code

- From the network the server obeys only signed requests (`src/auth.rs`): `/api/v1` needs the signature of
  an approved machine's key pair, and an agent signs the request that opens its WebSocket.
- A machine's private key is in its `config.json`, encrypted with Windows DPAPI (`src/dpapi.rs`).
- Requests from the same computer need no key; those a web page could send are refused (`src/loopback.rs`).
- A machine id belongs to the key it first connected with. The server's own agent is approved by itself.
- A job names its scene by its BLAKE3 hash; an agent checks the hash of each scene it downloads.
- `/api/v1/scenes/from-path` from the network takes only files inside the `scene_folders` of `config.json`.
- Traffic is not encrypted: the README says OpenFarm is for a trusted local network only.
- `unsafe` is denied. It is allowed by name, with its reason, only where Windows is called: the `dpapi`
  and `agent::jobobj` modules and one function of `src/window.rs`.
- The farm's state is under one `std::sync::Mutex`; its guard is never held across an `.await`.
- The server turns away an agent whose `PROTOCOL` is not its own; the app's version is not compared.
- Output folders must be full paths: the server and the agent both refuse any other.

## Traps

- A test farm on one computer: one `--data` folder and one `--local-port` per instance, with `--role`,
  `--server`, `--server-port`, `--name` and `--headless`; then `tests/e2e.py` with the `dummy` engine. It
  tests neither approval nor the signatures of `/api/v1`: what comes from the same computer is trusted.
- 3ds Max and Blender run copies of `hosts/` and `integrations/` that the exe writes, not these files.
