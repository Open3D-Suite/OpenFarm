# SPDX-License-Identifier: GPL-3.0-or-later
"""OpenFarm for Blender: Properties > Output > OpenFarm.

Sending saves a copy of the scene (the open file is untouched; relative paths are kept as they are and the
farm points them at this file's folder), uploads it to the farm through the OpenFarm app running on this
computer, and creates the job.
"""

import json
import os
import tempfile
import threading
import urllib.error
import urllib.parse
import urllib.request
import uuid

import bpy

LOCAL = "http://127.0.0.1:%s" % os.environ.get("OPENFARM_LOCAL_PORT", "47102")
_status = {"text": "", "ok": True}


def call(path, body=None, data=None, headers=None):
    if body is not None:
        data = json.dumps(body).encode()
        headers = {"content-type": "application/json"}
    req = urllib.request.Request(
        LOCAL + path, data=data, headers=headers or {}, method="POST" if data is not None else "GET"
    )
    try:
        with urllib.request.urlopen(req, timeout=600) as r:
            return json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        raise RuntimeError(e.read().decode(errors="replace") or str(e))
    except urllib.error.URLError:
        raise RuntimeError("OpenFarm is not running on this computer. Open OpenFarm and try again.")


def _redraw():
    for window in bpy.context.window_manager.windows:
        for area in window.screen.areas:
            if area.type == "PROPERTIES":
                area.tag_redraw()
    return None


def _set_status(text, ok=True):
    _status["text"], _status["ok"] = text, ok
    bpy.app.timers.register(_redraw, first_interval=0.01)


class OpenFarmSettings(bpy.types.PropertyGroup):
    frames: bpy.props.StringProperty(
        name="Frames", description="Empty: the scene's frame range. Or 1-100, 1-240:4, 1,7,22"
    )
    folder: bpy.props.StringProperty(
        name="Output folder",
        subtype="DIR_PATH",
        description="A folder every machine of the farm can reach, e.g. \\\\server\\projects\\shot\\renders",
    )
    file_name: bpy.props.StringProperty(
        name="File name", default="frame_####", description="#### becomes the frame number"
    )
    file_format: bpy.props.EnumProperty(
        name="Format", items=[("exr", "OpenEXR", ""), ("png", "PNG", ""), ("jpg", "JPEG", "")]
    )
    device: bpy.props.EnumProperty(
        name="Cycles device",
        items=[
            ("OPTIX", "OptiX", ""),
            ("CUDA", "CUDA", ""),
            ("HIP", "HIP", ""),
            ("ONEAPI", "oneAPI", ""),
            ("CPU", "CPU", ""),
        ],
    )
    priority: bpy.props.IntProperty(name="Priority", default=50, min=0, max=100)
    chunk: bpy.props.IntProperty(name="Frames per task", default=0, min=0, description="0: automatic")
    paused: bpy.props.BoolProperty(name="Send paused", default=False)


class OPENFARM_OT_send(bpy.types.Operator):
    bl_idname = "openfarm.send"
    bl_label = "Send to OpenFarm"
    bl_description = "Send this scene to the OpenFarm render farm"

    def execute(self, context):
        scene = context.scene
        s = scene.openfarm
        engine = {"CYCLES": "cycles", "BLENDER_EEVEE": "eevee", "BLENDER_EEVEE_NEXT": "eevee"}.get(scene.render.engine)
        if engine is None:
            self.report({"ERROR"}, "OpenFarm renders Cycles and Eevee scenes")
            return {"CANCELLED"}
        if scene.camera is None:
            self.report({"ERROR"}, "The scene has no camera")
            return {"CANCELLED"}
        folder = bpy.path.abspath(s.folder).rstrip("\\/") if s.folder else ""
        if not folder:
            self.report({"ERROR"}, "Choose an output folder")
            return {"CANCELLED"}
        if len(folder) > 1 and folder[1] == ":" and folder[0].upper() == "C":
            self.report({"ERROR"}, "The output folder is on this computer's C: drive; other machines can't write there")
            return {"CANCELLED"}
        original = bpy.data.filepath
        file_name = os.path.basename(original) or "untitled.blend"
        snapshot = os.path.join(tempfile.gettempdir(), "OpenFarm", uuid.uuid4().hex, file_name)
        os.makedirs(os.path.dirname(snapshot), exist_ok=True)
        # relative_remap=False keeps '//textures/…' as it is; the farm resolves it against the original folder.
        bpy.ops.wm.save_as_mainfile(filepath=snapshot, copy=True, relative_remap=False, compress=True)
        frames = s.frames.strip() or (
            "%d-%d" % (scene.frame_start, scene.frame_end) + (":%d" % scene.frame_step if scene.frame_step > 1 else "")
        )
        spec = {
            "name": os.path.splitext(file_name)[0],
            "engine": engine,
            "frames": frames,
            "camera": scene.camera.name,
            "output": {"folder": folder, "name": s.file_name or "frame_####", "format": s.file_format},
            "priority": s.priority,
            "chunk": s.chunk or None,
            "device": s.device if engine == "cycles" else None,
            "settings": {},
            "host_version": bpy.app.version_string,
            "submitted_by": "%s (%s)" % (os.environ.get("USERNAME", ""), os.environ.get("COMPUTERNAME", "")),
            "paused": s.paused,
        }
        _set_status("Sending the scene to the farm…")

        def work():
            try:
                size = os.path.getsize(snapshot)
                q = urllib.parse.urlencode({"name": file_name, "original": original})
                with open(snapshot, "rb") as f:
                    spec["scene"] = call(
                        "/api/v1/scenes?" + q,
                        data=f,
                        headers={"content-type": "application/octet-stream", "content-length": str(size)},
                    )
                job = call("/api/v1/jobs", spec)
                _set_status("Sent: job %s" % job["id"])
            except Exception as e:  # noqa: BLE001
                _set_status(str(e), False)
            finally:
                try:
                    os.remove(snapshot)
                except OSError:
                    pass

        threading.Thread(target=work, daemon=True).start()
        return {"FINISHED"}


class OPENFARM_OT_open(bpy.types.Operator):
    bl_idname = "openfarm.open"
    bl_label = "Open OpenFarm"

    def execute(self, context):
        try:
            call("/local/show", {})
        except RuntimeError as e:
            self.report({"ERROR"}, str(e))
        return {"FINISHED"}


class OPENFARM_PT_panel(bpy.types.Panel):
    bl_label = "OpenFarm"
    bl_space_type = "PROPERTIES"
    bl_region_type = "WINDOW"
    bl_context = "output"

    def draw(self, context):
        layout = self.layout
        s = context.scene.openfarm
        layout.use_property_split = True
        layout.prop(s, "frames")
        layout.prop(s, "folder")
        layout.prop(s, "file_name")
        layout.prop(s, "file_format")
        if context.scene.render.engine == "CYCLES":
            layout.prop(s, "device")
        layout.prop(s, "priority")
        layout.prop(s, "chunk")
        layout.prop(s, "paused")
        row = layout.row()
        row.operator("openfarm.open", icon="WINDOW")
        row.operator("openfarm.send", icon="RENDER_ANIMATION")
        if _status["text"]:
            box = layout.box()
            box.alert = not _status["ok"]
            box.label(text=_status["text"])


CLASSES = (OpenFarmSettings, OPENFARM_OT_send, OPENFARM_OT_open, OPENFARM_PT_panel)


def register():
    for c in CLASSES:
        bpy.utils.register_class(c)
    bpy.types.Scene.openfarm = bpy.props.PointerProperty(type=OpenFarmSettings)


def unregister():
    del bpy.types.Scene.openfarm
    for c in reversed(CLASSES):
        bpy.utils.unregister_class(c)
