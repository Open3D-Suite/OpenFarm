# SPDX-License-Identifier: GPL-3.0-or-later
"""OpenFarm's warm Blender host. The agent starts it as

    blender -b --python blender_host.py -- --port <n> --token <t>

(with CUDA_VISIBLE_DEVICES set when the task has one GPU). It connects to the agent on 127.0.0.1:<n>,
says {"ev": "ready", "token": t} and then runs one JSON command per line, ending each with
{"ev": "done"} or {"ev": "error"}:

    {"op": "load", "scene": path, "original_dir": folder the artist had it in}
    {"op": "render", "engine": "cycles" | "eevee", "camera": name | null, "frames": [..],
     "files": {"12": path, ..}, "format": "exr", "resolution": [w, h] | null, "device": "OPTIX",
     "settings": {"samples": n, "persistent": true}}
    {"op": "quit"}
"""

import json
import os
import socket
import sys
import time
import traceback

import bpy

argv = sys.argv[sys.argv.index("--") + 1 :] if "--" in sys.argv else []
PORT = int(argv[argv.index("--port") + 1])
TOKEN = argv[argv.index("--token") + 1]

sock = socket.create_connection(("127.0.0.1", PORT))
stream = sock.makefile("rw", encoding="utf-8", newline="\n")


class SceneError(Exception):
    """A problem of the scene itself: it fails the same way on every machine, so the job stops."""


def send(**event):
    stream.write(json.dumps(event, default=str) + "\n")
    stream.flush()


FORMATS = {"exr": "OPEN_EXR", "png": "PNG", "jpg": "JPEG", "jpeg": "JPEG", "tif": "TIFF", "tiff": "TIFF"}
ENGINES = {"cycles": "CYCLES", "eevee": "BLENDER_EEVEE"}


def remap_relative(original_dir):
    """The scene now lives in the agent's cache, so paths relative to it ('//textures/x.png') are made
    absolute against the folder the artist had it in."""
    if not original_dir:
        return 0
    count = 0
    for collection in (
        bpy.data.images,
        bpy.data.sounds,
        bpy.data.movieclips,
        bpy.data.volumes,
        bpy.data.cache_files,
        bpy.data.fonts,
        bpy.data.libraries,
    ):
        for block in collection:
            path = getattr(block, "filepath", "")
            if not path or not path.startswith("//"):
                continue
            if os.path.exists(bpy.path.abspath(path)):
                continue
            new = os.path.normpath(os.path.join(original_dir, path[2:].replace("/", os.sep)))
            block.filepath = new
            if collection is bpy.data.libraries:
                try:
                    block.reload()
                except Exception:  # noqa: BLE001
                    pass
            count += 1
    return count


def load(cmd):
    t0 = time.time()
    bpy.ops.wm.open_mainfile(filepath=cmd["scene"], load_ui=False)
    moved = remap_relative(cmd.get("original_dir") or "")
    missing = []
    for img in bpy.data.images:
        if img.source in ("FILE", "SEQUENCE") and img.filepath and not img.packed_file:
            if not os.path.exists(bpy.path.abspath(img.filepath)):
                missing.append(img.filepath)
    text = "Loaded in %.1f s" % (time.time() - t0)
    if moved:
        text += "; %d relative path(s) pointed at the original folder" % moved
    send(ev="log", text=text)
    for m in missing[:20]:
        send(ev="log", text="WARNING missing image: %s" % m)


def set_device(scene, device):
    device = (device or "OPTIX").upper()
    if device == "CPU":
        scene.cycles.device = "CPU"
        return
    prefs = bpy.context.preferences.addons["cycles"].preferences
    prefs.compute_device_type = device
    prefs.get_devices()
    used = []
    for d in prefs.devices:
        d.use = d.type == device
        if d.use:
            used.append(d.name)
    if not used:
        raise RuntimeError("no %s device on this machine" % device)
    scene.cycles.device = "GPU"
    send(ev="log", text="Cycles on %s: %s" % (device, ", ".join(used)))


def render(cmd):
    scene = bpy.context.scene
    engine = ENGINES.get(cmd["engine"])
    if not engine:
        raise SceneError("Blender does not render %s" % cmd["engine"])
    scene.render.engine = engine
    settings = cmd.get("settings") or {}
    if engine == "CYCLES":
        set_device(scene, cmd.get("device"))
        scene.render.use_persistent_data = bool(settings.get("persistent", True))
        if settings.get("samples"):
            scene.cycles.samples = int(settings["samples"])
    elif settings.get("samples"):
        scene.eevee.taa_render_samples = int(settings["samples"])
    if cmd.get("camera"):
        cam = bpy.data.objects.get(cmd["camera"])
        if cam is None:
            raise SceneError("the camera '%s' is not in the scene" % cmd["camera"])
        scene.camera = cam
    if scene.camera is None:
        raise SceneError("the scene has no camera")
    res = cmd.get("resolution")
    if res:
        scene.render.resolution_x, scene.render.resolution_y = int(res[0]), int(res[1])
        scene.render.resolution_percentage = 100
    fmt = FORMATS.get((cmd.get("format") or "exr").lower(), "OPEN_EXR")
    scene.render.image_settings.file_format = fmt
    scene.render.use_file_extension = False
    files = cmd["files"]
    for frame in cmd["frames"]:
        path = files[str(frame)]
        if os.path.exists(path):
            os.remove(path)
        send(ev="frame_start", frame=frame)
        t0 = time.time()
        scene.frame_set(frame)
        scene.render.filepath = path
        bpy.ops.render.render(write_still=True)
        if os.path.exists(path) and os.path.getsize(path) > 0:
            send(ev="frame_done", frame=frame, secs=time.time() - t0)
        else:
            send(ev="frame_failed", frame=frame, error="Blender finished the frame but wrote no image")


send(ev="ready", token=TOKEN, version=bpy.app.version_string)
for line in stream:
    try:
        cmd = json.loads(line)
    except ValueError:
        continue
    op = cmd.get("op")
    try:
        if op == "load":
            load(cmd)
        elif op == "render":
            render(cmd)
        elif op == "quit":
            send(ev="done")
            break
        else:
            raise RuntimeError("unknown command %r" % op)
        send(ev="done")
    except SceneError as e:
        send(ev="error", msg=str(e), fatal=True)
    except Exception:  # noqa: BLE001
        send(ev="error", msg=traceback.format_exc())
sock.close()
