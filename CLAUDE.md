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
- `src/folders.rs`: where OpenFarm keeps its files, and carrying its old folder over at start.
- `src/open3d_settings.rs`: what every Open3D app shares, the language and the folders of each app, with
  the cases its tests read (`src/open3d_settings_cases.txt`).
- `ui/` (plain HTML, JavaScript and CSS, no build step; English and Spanish texts in `ui/app.js`),
  `hosts/*.py` (run inside 3ds Max and Blender) and `integrations/` (the submitters) are built into the exe.
- `tests/*.py`: checks outside `cargo test`, against a running farm; `tests/max_button.py` needs no
  farm, only a 3ds Max batch session, and `tests/blender_texts.py` neither farm nor Blender.

## Build and check

- Build: `cargo build --release --locked`. `.cargo/config.toml` links the Visual C++ runtime into the exe
  (`+crt-static`; the C code of aws-lc and SQLite follows it), so `OpenFarm.exe` needs no
  `VCRUNTIME140.dll`; it reads the ignored `.cargo/config.local.toml` when there is one, for a build
  folder of one computer.
- Checks: `tools\check.ps1` runs them all, and all must pass. It needs Python 3.11 or newer and the
  ruff that `ruff.toml` names. First the steps built into `tools/repo.py`: the version in every place,
  the licence line of every source (`ui/` too), the common lints, no path of somebody's computer. Then
  what `tools/repo.toml` lists:
  - `cargo fmt --all --check`, `ruff format --check .` and `ruff check .`
  - `tests/blender_texts.py`: every text of the Blender add-on has its Spanish
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`
  - `cargo test --workspace --locked` (109 tests; without `--workspace` the 18 of `crates/core` do not run)
  - only with `-Full`: `tests/max_button.py` in `3dsmaxbatch`; the other `tests/*.py` are left out
- CI: `.github/workflows/check.yml` runs `tools\check.ps1`, without `-Full`, on every push and pull request
  to `main`, on GitHub's Windows runner with Python 3.14.
- `.editorconfig`, `.gitattributes`, `rustfmt.toml`, `clippy.toml`, `ruff.toml`, `LICENSE`,
  `tools/check.ps1`, `tools/repo.py`, `tools/repo_package.py`, `tools/repo_release.py` and
  `tools/repo_msi.py` are the same in every Open3D repo, and `src/open3d_settings.rs` with
  `src/open3d_settings_cases.txt` in every repo that has them. Do not change them here.

## Version and release

- The version is in the root `Cargo.toml`; `python -I tools/repo.py version set X.Y.Z` also writes it in
  `Cargo.lock`, `crates/core/Cargo.toml`, the bundle's `PackageContents.xml`, `blender_manifest.toml` and
  the README's `**Version X.Y.Z**` line; `tools\check.ps1` fails if they disagree.
- Changes a user can notice go into `CHANGELOG.md` under `## In development`, in the same commit.
- `python -I tools/repo.py package` makes the release package as the `[package]` table of
  `tools/repo.toml` says: the version and its `## X.Y.Z` section in the CHANGELOG, the quick checks (the
  tests among them), `cargo build --release --locked` without this computer's folders in the exe
  (`/d1trimfile` for the bundled C code), the licences of the dependencies in `THIRD-PARTY-LICENSES.txt`,
  then `dist/openfarm-<version>-windows-x64.zip` (`OpenFarm.exe`, README, LICENSE, CHANGELOG) and
  `dist/SHA256SUMS.txt`. It refuses an exe that holds a path of the computer that built it, or whose file
  version or `--version` is not the version.
- `python -I tools/repo.py release X.Y.Z` turns `## In development` into `## X.Y.Z — <date>`, sets the
  version, commits `Release X.Y.Z`, packages, tags `vX.Y.Z` and prints the pushes to approve; run again
  once they are done, it makes the GitHub release as a draft. It never pushes. `--dry-run` tries it all
  and writes only to `dist/`.
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
- The language is the one every Open3D app shares, in `settings.json` (`%APPDATA%\Open3D`, or the folder
  `OPEN3D_SETTINGS_DIR` names). The local app reads it once at start (`SharedLanguage` in `src/local.rs`);
  with no preference the language of Windows decides: Spanish if it is Spanish, else English. The page
  gets it from `/local/info`; its Language choice (*This machine*, and the first-run page) writes the file
  through `POST /local/language` and changes at once. OpenFarm kept no language before, so nothing is
  carried over. A page the farm server gives another computer's browser follows that browser.
- The Blender add-on follows Blender's interface language instead, through Blender's own translation
  system: its Spanish is `SPANISH` in its `__init__.py`, which `tests/blender_texts.py` checks. The 3ds
  Max button is in English only.
- Its files, from `src/folders.rs`: `config.json` in `%APPDATA%\Open3D\OpenFarm`; the log, `webview`,
  `server` (the farm's database, scenes, previews, task logs) and `agent` in `%LOCALAPPDATA%\Open3D\OpenFarm`;
  `OPEN3D_SETTINGS_DIR` and `OPEN3D_DATA_DIR` move them. `--data` keeps everything in its one folder and
  copies nothing. `main` first takes the local port, read from the `config.json` it will use: a second
  start hands over to the first and copies nothing. Then, before the log, the settings or the database
  open, it copies from the old `%LOCALAPPDATA%\OpenFarm` (`%TEMP%\OpenFarm` without `LOCALAPPDATA`), once
  and leaving it as it was: `config.json` (by the rules of `carry_over`, which copies only whole folders)
  and `server` with `open3d_settings::carry_over`, holding `farm.sqlite` so that nothing writes it meanwhile,
  and not at all while a program has it open to write. The log, `webview` and `agent` are made again. A
  failed copy leaves that run on the old folder for what was not copied, and the next start tries again;
  the log says why and `/local/info` gives it to the page (`not_copied`). Only debug builds read
  `OPENFARM_TEST_OLD_FOLDER`, a folder that stands for the old one in a test.
- `unsafe` is denied. It is allowed by name, with its reason, only where Windows is called: the `dpapi`
  and `agent::jobobj` modules, one function of `src/window.rs` and `user_interface_language` in
  `src/open3d_settings.rs`.
- The farm's state is under one `std::sync::Mutex`; its guard is never held across an `.await`.
- The server turns away an agent whose `PROTOCOL` is not its own; the app's version is not compared.
- Output folders must be full paths: the server and the agent both refuse any other.

## Traps

- A test farm on one computer: one `--data` folder and one `--local-port` per instance, with `--role`,
  `--server`, `--server-port`, `--name` and `--headless`; then `tests/e2e.py` with the `dummy` engine. It
  tests neither approval nor the signatures of `/api/v1`: what comes from the same computer is trusted.
- 3ds Max and Blender run copies of `hosts/` and `integrations/` that the exe writes, not these files.
