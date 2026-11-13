# Changelog

What changed for users in each version of OpenFarm, newest first.

## In development

The first version: a render farm for 3ds Max and Blender on your own computers.

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
