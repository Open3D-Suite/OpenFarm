# OpenFarm

A render farm for your own computers. Send a scene from **3ds Max** or **Blender** and OpenFarm renders it
on every machine of the office or the studio, checks every frame and shows the progress in one window.
No accounts, no licence keys, no cloud. Part of the [Open3D](https://github.com/carlesllavata/Open3D)
suite of open-source tools for 3D work.

**Version 0.1.1** – the first release, packaged again.

Engines: **Corona**, **V-Ray**, **V-Ray GPU** and **FStorm** in 3ds Max; **Cycles** (CPU, CUDA, OptiX, HIP,
oneAPI) and **Eevee** in Blender.

## What it does

- **One program, three roles.** One computer runs the farm (it keeps the queue and hands out work, and
  can render too); the others join it to render and to send jobs. Machines find the farm on the network
  by themselves.
- **Scenes stay loaded.** Each machine keeps 3ds Max or Blender open between tasks of the same job, so a
  scene is opened once per machine, not once per frame. 3ds Max takes 25–60 seconds just to start;
  a heavy Blender scene renders more than 5 times faster per frame this way.
- **Errors show up in the first minute.** The first, last and middle frames render first, then the gaps
  are filled by halves, so a broken scene is caught at once and the whole animation can be previewed
  early.
- **Every frame is checked.** A frame that was not written, is empty, black or has broken pixels is
  reported, not counted as done. Each frame gets a preview in the window.
- **Work is shared fairly.** Higher priority first; equal jobs share the machines; a machine that already
  has a job's scene open keeps that job. Tasks are sized automatically (single frames first, then about
  five minutes of work each, never so big that other machines are left idle).
- **Failures don't stop the farm.** A failed frame is retried on another machine (up to 3 times); a
  machine that fails a job twice stops getting it; a problem of the scene itself (no camera, the wrong
  renderer) stops the job at once with a clear message.
- **The scene is a snapshot.** Sending a job uploads a copy of the scene to the farm, so the artist can
  keep working on it. Paths relative to the scene's folder still work.
- **Machines join with approval.** A new machine appears as "waiting for approval" until someone approves
  it on the farm. The farm's window and API can only be used from this computer or from approved
  machines, which sign everything they ask for (see [Security](#security)).

## Install

OpenFarm is a single program for Windows 10/11 (64-bit), installed on every computer of the farm. Two
ways:

- **With the [Open3D launcher](https://github.com/carlesllavata/Open3D)** (0.3.3 or later): press
  *Install* on OpenFarm's card. It goes in `%LOCALAPPDATA%\Programs\Open3D\OpenFarm`, with shortcuts in
  the Start menu and on the desktop, and the launcher updates it and removes it.
- **By hand**: download `openfarm-<version>-windows-x64.zip` from the
  [releases page](https://github.com/carlesllavata/OpenFarm/releases/latest), unzip it anywhere and run
  `OpenFarm.exe`.

Every computer of a farm should run the same version of OpenFarm.

The first time, OpenFarm asks what this computer does:

- **This computer runs the farm**: give the farm a name; choose whether it renders too.
- **Join a farm**: press *Search the network* and pick the farm (or type the server's address), then
  choose whether this is a workstation or a dedicated render node. On the farm's computer, approve the
  new machine in *Machines*.

Closing the window can leave OpenFarm working in the background; opening OpenFarm again brings the
window back. *This machine > Quit OpenFarm* stops it.

### Sending jobs

- **3ds Max**: *This machine > Install the 3ds Max button*, restart 3ds Max, then add **Send to OpenFarm**
  (Customize User Interface, category *OpenFarm*) to a toolbar or menu. The window takes the camera, frames
  and output from the scene.
- **Blender** (4.2 or later): *This machine > Install the Blender add-on*, restart Blender, then
  *Properties > Output > OpenFarm*.
- **Without opening the scene**: the *Submit* page takes a scene path the farm's computer can read. From
  another computer this needs the scene's folder to be allowed on the farm (see [Security](#security)).

The scene's textures and the output folder must be on paths **every machine can reach** (a shared folder
such as `\\server\projects`), not on a local disk. OpenFarm refuses output folders that are not full
paths.

### What each machine needs

- The render software of the jobs it should take, installed and licensed: 3ds Max 2025 or later with
  the renderer, or Blender. OpenFarm finds them by itself and only gives a machine jobs it can render.
- Renderer licences for rendering without a user interface: V-Ray asks for a **render node** licence
  when 3ds Max renders in batch mode.
- An NVIDIA GPU for GPU engines (V-Ray GPU, FStorm, Cycles on CUDA/OptiX, Eevee). Each GPU takes its own
  task.

## Limits of this version

- Distributed rendering of single frames (V-Ray and Corona DR) is not there yet; every frame renders
  on one machine.
- Workstations render whenever they are active: there is no idle detection or schedule yet (suspend a
  machine from its window or from *Machines*).
- Tested so far with Corona and Cycles. V-Ray, V-Ray GPU and FStorm are supported but were not tried on a
  licensed machine yet, and Eevee not through the farm yet: report what you find.
- The connection between machines is not encrypted; use OpenFarm on a trusted local network only (see
  [Security](#security)).
- Windows only.

## Network and files

- TCP **47100**: the farm (on the farm's computer). UDP **47101**: finding the farm. TCP **47102**: the
  window and the 3ds Max / Blender buttons (this computer only). Windows may ask to let OpenFarm through
  its firewall on private networks.
- Settings, the farm's database, the scene snapshots and the caches live in
  `%LOCALAPPDATA%\OpenFarm` (`--data <folder>` uses another one).

## Security

OpenFarm is made for a local network you trust. What it protects, and what it does not:

- **Only approved machines can use the farm.** Requests from the farm's own computer are trusted. From the
  network the farm only listens to machines it has approved. Each machine makes its own key pair the
  first time it starts. The private key never leaves the machine: every request is signed with it, the
  signature is good for that one request only, and a copy of a request sent again is refused. The farm
  keeps just a fingerprint of each machine's public key, so nothing in its database lets anybody pass
  for a machine. A machine's place in the farm belongs to the key it first joined with: whoever comes
  as that machine with another key is turned away, and the machine keeps its approval and its
  connection.
- **The key is protected on each machine.** It is stored in `config.json` encrypted by Windows for the
  user who runs OpenFarm (DPAPI). A copy of the file is useless on another computer or for another
  user: OpenFarm started from such a copy makes a new key, and the farm turns it away because it
  knows that machine by the old one.
- **A machine that lost its key joins again as a new one.** When OpenFarm is installed again or runs
  as another Windows user, the machine has a new key and the farm does not take it in place of the
  old one; its window says so. On the farm's computer, *Forget* the machine in *Machines*: it then
  joins as a new machine, waiting for approval.
- **Clocks must agree.** A signature is accepted for five minutes, so the clocks of the farm's computers
  must be within five minutes of each other (Windows keeps them in time by itself). A machine whose
  clock is off is told so.
- **Scenes by path come from allowed folders only.** The *Submit* page makes the farm's computer read the
  scene file itself. From another computer of the farm that only works for files inside the folders
  listed under `scene_folders` in the `config.json` of the farm's computer, for example
  `"scene_folders": ["\\\\server\\projects", "D:\\scenes"]` (quit OpenFarm, edit the file, start it
  again). The list is empty at first: other computers send scenes from 3ds Max or Blender, or from
  *Submit* on the farm's computer, which takes any path.
- **Web pages are kept out.** On each computer OpenFarm answers that computer's own programs without a
  key: its window, the 3ds Max and Blender buttons, scripts. A page open in a browser is not one of
  them. What a browser sends for a page of another site, or to another name than `127.0.0.1` or
  `localhost`, is refused before OpenFarm does anything with it.
- **Not protected: the traffic itself.** Nothing is encrypted. Scenes, previews, logs and commands
  travel in clear, so anybody on the network can read them, and somebody able to change traffic on its
  way can still tamper with what is not signed (the farm's answers, the messages of a machine once it
  is connected). Scenes are checked all the same: a machine compares the scene it downloaded with the
  fingerprint its job names and refuses to render one that was changed. Use OpenFarm only on a local
  network you trust:
  **do not expose it to the internet and do not forward its ports on the router.**
- **Not protected: a computer of the farm from its own programs.** Any program running on a computer of
  the farm can use the farm as that computer does.

## Uninstall

Quit OpenFarm (*This machine > Quit OpenFarm*), then remove it with the launcher (or delete its folder),
and delete `%LOCALAPPDATA%\OpenFarm`,
`%APPDATA%\Autodesk\ApplicationPlugins\OpenFarm.bundle` and the Blender add-on
(*Preferences > Get Extensions > OpenFarm > Uninstall*).

## Building

Rust (stable) on Windows: `cargo test --workspace`, `cargo build --release`.
`python -I tools/package.py` builds, tests and writes `dist/openfarm-<version>-windows-x64.zip` with
`SHA256SUMS.txt`. `tests/e2e.py` sends a job to a running farm and checks the result (the test
renderer needs no 3D software).

## License

OpenFarm is free software under the **GNU General Public License v3.0 or later**
([LICENSE](LICENSE)). The libraries it is built with and their licences are listed in
`THIRD-PARTY-LICENSES.txt` inside each release.
