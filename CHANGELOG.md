# Changelog

What changed for users in each version of OpenFarm, newest first.

## In development

**Not compatible with 0.1.1.** Update every computer of the farm at the same time, then approve the
machines again on the farm's computer (*Machines*).

- **Machines no longer send a secret key.** Until now every machine sent its key in clear with each
  request and the farm kept all the keys in its database, so anybody who could read the network or that
  file could act as a machine. Now each machine has a key pair: it signs every request, the private key
  never leaves it, and the farm stores only a fingerprint. A copied request is refused, and so is one
  signed more than five minutes away from the farm's clock (the message says the clocks differ).
- **The key is protected on each machine.** It is kept encrypted by Windows for the user who runs
  OpenFarm. The plain key of 0.1.1 is removed from the settings, and the keys a 0.1.1 farm stored are
  erased from its database, the first time the new version starts.
- **Scenes by path only from allowed folders.** *Submit* from another computer of the farm now works
  only for scenes inside the folders listed under `scene_folders` in the farm computer's
  `config.json` (none at first). From the farm's own computer, and from 3ds Max and Blender, nothing
  changes.
- **A changed upload is noticed.** A scene that does not reach the farm exactly as it was sent is
  reported instead of being used.
- **Clearer messages when versions do not match.** A 0.1.1 machine that tries to join gets "Upgrade
  Required", and a machine turned away by the farm shows the farm's reason.

- **Web pages cannot use OpenFarm behind your back.** OpenFarm answers the programs of its own computer
  without a key. A page open in a browser on that computer could send it requests too: upload files to
  the farm, quit OpenFarm and, with a trick, reach everything else. Requests from the computer itself
  are now served only to OpenFarm's window, the 3ds Max and Blender buttons and scripts.

- **A machine cannot be pushed aside by another one.** A machine's place in the farm now belongs to the
  key it first joined with. A computer that joins claiming to be a machine the farm already knows, with
  another key, is turned away and told why; the real machine keeps its approval and its connection, and
  *Approve* can no longer approve the newcomer in its place. A machine that really lost its key joins
  again once it is forgotten in *Machines*.

- **Machines check the scenes they download.** Before opening a scene, a machine makes sure that what
  it downloaded, or kept from an earlier job, is exactly the scene its job names. A scene that was
  changed or damaged on the way is deleted and the task fails saying so, instead of rendering it.

- **An open window no longer asks the farm without a pause.** The window is meant to wait until
  something changes on the farm. Since the first version it asked again at once, about a thousand times
  a second for as long as it was open. It waits now.

- **The "Send to OpenFarm" button of 3ds Max opens its window.** Until now, pressing it showed a script
  error ("SyntaxError") and no window. It also works when the Windows user's folder has spaces, accents
  or an apostrophe in its name.

To get this fix, install the button again (*This machine > Install the 3ds Max button*) and restart
3ds Max.

The traffic between machines is still not encrypted: see *Security* in the README.

## 0.1.1 — 16 November 2026

- **The program no longer names folders of the computer it was built on.** Earlier builds
  carried the full paths of their source files inside `OpenFarm.exe`. Nothing else changes.

## 0.1.0 — 13 November 2026

The first release: a render farm for 3ds Max and Blender on your own computers. Tested with Corona
and Cycles; V-Ray, V-Ray GPU, FStorm and Eevee are supported but not tried through the farm yet.

- **One program for the whole farm.** The first start asks whether this computer runs the farm or joins
  one; machines find the farm on the network and join after the farm approves them.
- **3ds Max and Blender.** Corona, V-Ray, V-Ray GPU and FStorm scenes from 3ds Max; Cycles (CPU, CUDA,
  OptiX, HIP, oneAPI) and Eevee from Blender. Each machine only gets the jobs its software can render,
  and every NVIDIA GPU takes its own task.
- **Fast on every machine.** 3ds Max and Blender stay open between tasks of the same job, so the scene is
  loaded once per machine.
- **Smart frame order.** First, last and middle frames first, then the gaps by halves: problems show up
  in the first minute and the animation can be previewed early.
- **Checked frames with previews.** Missing, empty, black or broken frames are reported; every frame
  gets a preview in the job's page.
- **Retries and clear failures.** Failed frames are retried on other machines; a machine that keeps
  failing a job stops getting it; scene problems (no camera, wrong renderer) stop the job with a message.
- **Send from 3ds Max, Blender or the window.** A "Send to OpenFarm" button for 3ds Max and an add-on for
  Blender, both installed from the window, and a page to send a scene without opening it.
- **The farm in one window**, also reachable from the browsers of approved machines: jobs with progress,
  time left, priority, pause, cancel and retry; each job's frames, tasks and logs; every machine with
  its CPU, memory, GPUs, software and running work, to approve, suspend or forget.
