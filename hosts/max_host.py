# SPDX-License-Identifier: GPL-3.0-or-later
"""OpenFarm's warm 3ds Max host. The agent starts it as

    3dsmaxbatch.exe max_host.py -mxsString port:<n> -mxsString token:<t> -dm on -v 3 -listenerlog <file>

It connects to the agent on 127.0.0.1:<n>, says {"ev": "ready", "token": t} and then runs one JSON command
per line, answering with events and ending every command with {"ev": "done"} or {"ev": "error"}:

    {"op": "load", "scene": path, "original_dir": folder the artist had it in}
    {"op": "render", "engine": "corona", "camera": name, "frames": [..], "files": {"12": path, ..},
     "resolution": [w, h] | null, "settings": {"renderer_props": {...}}}
    {"op": "quit"}

Max stays open between commands, so a job's scene is loaded once per machine, not once per frame.
"""

import json
import os
import socket
import threading
import time
import traceback

from pymxs import runtime as rt

args = rt.maxOps.mxsCmdLineArgs
PORT = int(args[rt.Name("port")])
TOKEN = str(args[rt.Name("token")])

sock = socket.create_connection(("127.0.0.1", PORT))
stream = sock.makefile("rw", encoding="utf-8", newline="\n")
lock = threading.Lock()


class SceneError(Exception):
    """A problem of the scene itself: it fails the same way on every machine, so the job stops."""


def send(**event):
    with lock:
        stream.write(json.dumps(event, default=str) + "\n")
        stream.flush()


# Renderer class names change with every version (V_Ray_7__update_4_hotfix_2_DR2), so they are matched
# by what they start with.
ENGINE_CLASS = {
    "vray": lambda n: n.startswith("v_ray_") and not n.startswith("v_ray_gpu"),
    "vray_gpu": lambda n: n.startswith("v_ray_gpu"),
    "corona": lambda n: n.startswith("corona"),
    "fstorm": lambda n: "fstorm" in n,
}


def renderer_name():
    return str(rt.classOf(rt.renderers.current))


def load(cmd):
    rt.resetMaxFile(rt.Name("noPrompt"))
    original = cmd.get("original_dir") or ""
    if original:
        # Bitmaps saved with paths relative to the scene's own folder are found there.
        # On purpose: if the folder cannot be added, the scene is loaded all the same.
        try:
            rt.mapPaths.add(original)
        except Exception:  # noqa: BLE001, S110
            pass
    t0 = time.time()
    ok = rt.loadMaxFile(cmd["scene"], quiet=True, useFileUnits=True)
    if not ok:
        raise RuntimeError("3ds Max could not open the scene (a missing plug-in or a damaged file?)")
    send(ev="log", text="Loaded in %.1f s; renderer %s" % (time.time() - t0, renderer_name()))


def find_camera(name):
    if name:
        cam = rt.getNodeByName(name)
        if cam is None:
            raise SceneError("the camera '%s' is not in the scene" % name)
        return cam
    cam = rt.viewport.getCamera()
    if cam is None:
        raise SceneError("the job has no camera and the active view is not a camera")
    return cam


def render(cmd):
    engine = cmd["engine"]
    test = ENGINE_CLASS.get(engine)
    if test and not test(renderer_name().lower()):
        raise SceneError("the scene renders with %s, not %s" % (renderer_name(), engine))
    cam = find_camera(cmd.get("camera"))
    res = cmd.get("resolution")
    if res:
        rt.renderWidth, rt.renderHeight = int(res[0]), int(res[1])
    settings = cmd.get("settings") or {}
    r = rt.renderers.current
    for name, value in (settings.get("renderer_props") or {}).items():
        rt.setProperty(r, rt.Name(name), value)
    files = cmd["files"]
    for frame in cmd["frames"]:
        path = files[str(frame)]
        if os.path.exists(path):
            os.remove(path)
        send(ev="frame_start", frame=frame)
        t0 = time.time()
        bmp = rt.render(
            camera=cam,
            frame=frame,
            outputfile=path,
            vfb=False,
            quiet=True,
            progressbar=False,
            missingExtFilesAction=rt.Name("logmsg"),
            missingXRefsAction=rt.Name("logmsg"),
            missingDLLsAction=rt.Name("logmsg"),
        )
        if bmp is not None:
            # On purpose: whether the frame was rendered is decided below, by the file it wrote.
            try:
                rt.close(bmp)
            except Exception:  # noqa: BLE001, S110
                pass
        if os.path.exists(path) and os.path.getsize(path) > 0:
            send(ev="frame_done", frame=frame, secs=time.time() - t0)
        else:
            # Phase 0: a renderer without a licence returns from render() without writing anything.
            send(
                ev="frame_failed",
                frame=frame,
                error="3ds Max finished the frame but wrote no image (no renderer licence? see the log)",
            )


send(ev="ready", token=TOKEN, version=list(rt.maxVersion()))
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
