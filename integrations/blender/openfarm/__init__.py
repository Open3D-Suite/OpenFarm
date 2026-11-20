# SPDX-License-Identifier: GPL-3.0-or-later
"""OpenFarm for Blender: Properties > Output > OpenFarm.

Sending saves a copy of the scene (the open file is untouched; relative paths are kept as they are and the
farm points them at this file's folder), uploads it to the farm through the OpenFarm app running on this
computer, and creates the job.

The texts are in English and, through Blender's own translation system, in Spanish (SPANISH): they follow
Blender's interface language and its Translate options, like Blender's own texts, not the language the
Open3D apps share (D-050).
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
from bpy.app.translations import pgettext_iface as iface_
from bpy.app.translations import pgettext_rpt as rpt_

LOCAL = "http://127.0.0.1:%s" % os.environ.get("OPENFARM_LOCAL_PORT", "47102")
# What the panel says under the buttons: an English text, which draw() translates, and its values.
_status = {"text": "", "values": (), "ok": True}

# Every text of the add-on in Spanish, with the words of OpenFarm's own window (ui/app.js). The names of
# formats, devices and engines are not translated. tests/blender_texts.py checks that none is missing.
SPANISH = {
    "Frames": "Fotogramas",
    "Empty: the scene's frame range. Or 1-100, 1-240:4, 1,7,22": (
        "Vacío: los fotogramas de la escena. O 1-100, 1-240:4 (uno de cada 4), 1,7,22"
    ),
    "Output folder": "Carpeta de salida",
    "A folder every machine of the farm can reach, e.g. \\\\server\\projects\\shot\\renders": (
        "Una carpeta que vean todas las máquinas de la granja, p. ej. \\\\servidor\\proyectos\\plano\\renders"
    ),
    "File name": "Nombre de archivo",
    "#### becomes the frame number": "#### se cambia por el número de fotograma",
    "Format": "Formato",
    "Cycles device": "Dispositivo de Cycles",
    "Priority": "Prioridad",
    "Frames per task": "Fotogramas por tarea",
    "0: automatic": "0: automático",
    "Send paused": "Enviar en pausa",
    "Send to OpenFarm": "Enviar a OpenFarm",
    "Send this scene to the OpenFarm render farm": "Envía esta escena a la granja de render OpenFarm",
    "Open OpenFarm": "Abrir OpenFarm",
    "Open the OpenFarm window of this computer": "Abre la ventana de OpenFarm de este equipo",
    "OpenFarm renders Cycles and Eevee scenes": "OpenFarm renderiza escenas de Cycles y Eevee",
    "The scene has no camera": "La escena no tiene cámara",
    "Choose an output folder": "Elige una carpeta de salida",
    "The output folder is on this computer's C: drive; other machines can't write there": (
        "La carpeta de salida está en la unidad C: de este equipo; las demás máquinas no pueden escribir ahí"
    ),
    "Sending the scene to the farm…": "Enviando la escena a la granja…",
    "Sent: job %s": "Trabajo %s enviado",
    "OpenFarm is not running on this computer. Open OpenFarm and try again.": (
        "OpenFarm no está abierto en este equipo. Ábrelo y vuelve a intentarlo."
    ),
}


def translations():
    """SPANISH for bpy.app.translations.register: Blender looks a button's label up in the "Operator"
    context and the other texts in the default one, "*"."""
    return {"es": {(context, english): text for english, text in SPANISH.items() for context in ("*", "Operator")}}


def call(path, body=None, data=None, headers=None):
    if body is not None:
        data = json.dumps(body).encode()
        headers = {"content-type": "application/json"}
    # The address always starts with LOCAL: the OpenFarm of this computer, over http.
    req = urllib.request.Request(  # noqa: S310
        LOCAL + path, data=data, headers=headers or {}, method="POST" if data is not None else "GET"
    )
    try:
        with urllib.request.urlopen(req, timeout=600) as r:  # noqa: S310
            return json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        raise RuntimeError(e.read().decode(errors="replace") or str(e)) from e
    except urllib.error.URLError as e:
        raise RuntimeError("OpenFarm is not running on this computer. Open OpenFarm and try again.") from e


def _redraw():
    for window in bpy.context.window_manager.windows:
        for area in window.screen.areas:
            if area.type == "PROPERTIES":
                area.tag_redraw()
    return None


def _set_status(text, *values, ok=True):
    """`text` in English (or as the farm sent it), with the values of its %s; draw() translates it."""
    _status["text"], _status["values"], _status["ok"] = text, values, ok
    bpy.app.timers.register(_redraw, first_interval=0.01)


def _status_text():
    text = iface_(_status["text"])
    return text % _status["values"] if _status["values"] else text


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
            self.report({"ERROR"}, rpt_("OpenFarm renders Cycles and Eevee scenes"))
            return {"CANCELLED"}
        if scene.camera is None:
            self.report({"ERROR"}, rpt_("The scene has no camera"))
            return {"CANCELLED"}
        folder = bpy.path.abspath(s.folder).rstrip("\\/") if s.folder else ""
        if not folder:
            self.report({"ERROR"}, rpt_("Choose an output folder"))
            return {"CANCELLED"}
        if len(folder) > 1 and folder[1] == ":" and folder[0].upper() == "C":
            self.report(
                {"ERROR"}, rpt_("The output folder is on this computer's C: drive; other machines can't write there")
            )
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
                _set_status("Sent: job %s", job["id"])
            except Exception as e:  # noqa: BLE001
                _set_status(str(e), ok=False)
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
    bl_description = "Open the OpenFarm window of this computer"

    def execute(self, context):
        try:
            call("/local/show", {})
        except RuntimeError as e:
            self.report({"ERROR"}, rpt_(str(e)))
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
            box.label(text=_status_text(), translate=False)


CLASSES = (OpenFarmSettings, OPENFARM_OT_send, OPENFARM_OT_open, OPENFARM_PT_panel)


def register():
    bpy.app.translations.register(__name__, translations())
    for c in CLASSES:
        bpy.utils.register_class(c)
    bpy.types.Scene.openfarm = bpy.props.PointerProperty(type=OpenFarmSettings)


def unregister():
    del bpy.types.Scene.openfarm
    for c in reversed(CLASSES):
        bpy.utils.unregister_class(c)
    bpy.app.translations.unregister(__name__)
