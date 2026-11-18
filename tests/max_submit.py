# SPDX-License-Identifier: GPL-3.0-or-later
"""Checks the 3ds Max submitter without its window, in a 3ds Max batch session (never in an artist's Max):

    set OPENFARM_LOCAL_PORT=47202
    3dsmaxbatch.exe tests/max_submit.py -mxsString scene:<file.max> -mxsString bundle:<...\\Contents\\scripts>
        -mxsString out:<output folder>

Prints what the window would show, then sends the scene the way the window does.
"""

import os
import sys
import tempfile
import uuid

from pymxs import runtime as rt

args = rt.maxOps.mxsCmdLineArgs
scene, bundle, out = (str(args[rt.Name(k)]) for k in ("scene", "bundle", "out"))
sys.path.insert(0, bundle)
import openfarm_max as ofm  # noqa: E402

rt.loadMaxFile(scene, quiet=True)
print("ENGINE", ofm.engine_of_scene())
print("CAMERAS", ofm.scene_cameras())
print("FRAMES", ofm.scene_frames())
print("OUTPUT", ofm.default_output())
dirty_before = rt.getSaveRequired()
snapshot = os.path.join(tempfile.gettempdir(), "OpenFarm", uuid.uuid4().hex, str(rt.maxFileName))
os.makedirs(os.path.dirname(snapshot), exist_ok=True)
ok = ofm.save_copy(snapshot)
print(
    "SNAPSHOT",
    ok,
    os.path.getsize(snapshot),
    "file still",
    rt.maxFileName,
    "dirty kept",
    rt.getSaveRequired() == dirty_before,
)
with open(snapshot, "rb") as f:
    import urllib.parse

    q = urllib.parse.urlencode({"name": str(rt.maxFileName), "original": scene})
    ref = ofm.call(
        "/api/v1/scenes?" + q,
        data=f,
        headers={"content-type": "application/octet-stream", "content-length": str(os.path.getsize(snapshot))},
    )
print("SCENE", ref)
job = ofm.call(
    "/api/v1/jobs",
    {
        "name": "max submitter test",
        "engine": ofm.engine_of_scene(),
        "scene": ref,
        "frames": "0-3",
        "camera": ofm.scene_cameras()[0],
        "output": {"folder": out, "name": "maxsub_####", "format": "exr"},
        "settings": {},
        "submitted_by": "max_submit test",
    },
)
print("JOB", job)
