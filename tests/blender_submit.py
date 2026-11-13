# SPDX-License-Identifier: GPL-3.0-or-later
"""Checks the Blender add-on without a UI: registers it, sends the open scene and waits for the answer.

    set OPENFARM_LOCAL_PORT=47202
    blender -b scene.blend --python tests/blender_submit.py -- <integrations/blender folder> <output folder>
"""
import sys
import time

import bpy

addon_dir, out = sys.argv[sys.argv.index("--") + 1:][:2]
sys.path.insert(0, addon_dir)
import openfarm  # noqa: E402

openfarm.register()
s = bpy.context.scene.openfarm
s.frames = "1-4"
s.folder = out
s.file_name = "addon_####"
s.file_format = "png"
result = bpy.ops.openfarm.send()
print("operator:", result)
deadline = time.time() + 120
while time.time() < deadline and (not openfarm._status["text"] or openfarm._status["text"].startswith("Sending")):
    time.sleep(0.2)
print("STATUS:", openfarm._status)
openfarm.unregister()
