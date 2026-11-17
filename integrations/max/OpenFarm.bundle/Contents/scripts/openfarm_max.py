# SPDX-License-Identifier: GPL-3.0-or-later
"""Send to OpenFarm, for 3ds Max.

A small window with the job's settings, filled from the scene. Sending saves a copy of the scene (the
open file is not touched and keeps its unsaved changes flag), uploads the copy to the farm through the
OpenFarm app running on this computer, and creates the job. The upload runs in the background so 3ds Max
stays usable.
"""

import json
import os
import tempfile
import threading
import urllib.error
import urllib.parse
import urllib.request
import uuid

from pymxs import runtime as rt
from PySide6 import QtCore, QtWidgets

LOCAL = "http://127.0.0.1:%s" % os.environ.get("OPENFARM_LOCAL_PORT", "47102")
_window = None


def engine_of_scene():
    name = str(rt.classOf(rt.renderers.current)).lower()
    if name.startswith("corona"):
        return "corona"
    if name.startswith("v_ray_gpu"):
        return "vray_gpu"
    if name.startswith("v_ray_"):
        return "vray"
    if "fstorm" in name:
        return "fstorm"
    return None


def scene_cameras():
    return [c.name for c in rt.cameras if rt.superClassOf(c) == rt.camera]


def scene_frames():
    kind = rt.rendTimeType
    if kind == 1:
        return str(int(rt.sliderTime.frame))
    if kind == 2:
        return "%d-%d" % (int(rt.animationRange.start.frame), int(rt.animationRange.end.frame))
    if kind == 3:
        step = int(getattr(rt, "rendNThFrame", 1) or 1)
        frames = "%d-%d" % (int(rt.rendStart.frame), int(rt.rendEnd.frame))
        return frames + (":%d" % step if step > 1 else "")
    return str(rt.rendPickupFrames).replace(" ", "") or "1"


def default_output():
    out = str(rt.rendOutputFilename or "")
    if out:
        folder, name = os.path.split(out)
        base, ext = os.path.splitext(name)
        fmt = ext.lstrip(".").lower() if ext.lower() in (".exr", ".png", ".jpg", ".jpeg") else "exr"
        return folder, (base or "frame") + "_####", "jpg" if fmt == "jpeg" else fmt
    folder = os.path.join(str(rt.maxFilePath), "renders") if rt.maxFilePath else ""
    return folder, os.path.splitext(str(rt.maxFileName) or "frame")[0] + "_####", "exr"


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


class Sender(QtCore.QObject):
    finished = QtCore.Signal(str, bool)

    def send(self, snapshot, file_name, original, spec):
        def work():
            try:
                size = os.path.getsize(snapshot)
                with open(snapshot, "rb") as f:
                    q = urllib.parse.urlencode({"name": file_name, "original": original})
                    scene = call(
                        "/api/v1/scenes?" + q,
                        data=f,
                        headers={"content-type": "application/octet-stream", "content-length": str(size)},
                    )
                spec["scene"] = scene
                job = call("/api/v1/jobs", spec)
                self.finished.emit("Sent: job %s" % job["id"], True)
            except Exception as e:  # noqa: BLE001
                self.finished.emit(str(e), False)
            finally:
                try:
                    os.remove(snapshot)
                except OSError:
                    pass

        threading.Thread(target=work, daemon=True).start()


class Window(QtWidgets.QWidget):
    def __init__(self, parent=None):
        super().__init__(parent, QtCore.Qt.Window)
        self.setWindowTitle("Send to OpenFarm")
        self.setMinimumWidth(520)
        form = QtWidgets.QFormLayout()
        self.name = QtWidgets.QLineEdit()
        self.engine = QtWidgets.QLabel()
        self.camera = QtWidgets.QComboBox()
        self.frames = QtWidgets.QLineEdit()
        self.frames.setToolTip("1-100, 1-240:4 (every 4th) or 1,7,22")
        self.folder = QtWidgets.QLineEdit()
        self.folder.setToolTip("A folder every machine of the farm can reach, e.g. \\\\server\\projects\\shot\\renders")
        self.file = QtWidgets.QLineEdit()
        self.format = QtWidgets.QComboBox()
        self.format.addItems(["exr", "png", "jpg"])
        self.priority = QtWidgets.QSpinBox()
        self.priority.setRange(0, 100)
        self.priority.setValue(50)
        self.chunk = QtWidgets.QSpinBox()
        self.chunk.setRange(0, 1000)
        self.chunk.setSpecialValueText("Automatic")
        self.paused = QtWidgets.QCheckBox("Send paused")
        for label, w in [
            ("Job name", self.name),
            ("Engine", self.engine),
            ("Camera", self.camera),
            ("Frames", self.frames),
            ("Output folder", self.folder),
            ("File name", self.file),
            ("Format", self.format),
            ("Priority", self.priority),
            ("Frames per task", self.chunk),
            ("", self.paused),
        ]:
            form.addRow(label, w)
        self.message = QtWidgets.QLabel()
        self.message.setWordWrap(True)
        self.send_btn = QtWidgets.QPushButton("Send to the farm")
        self.send_btn.clicked.connect(self.on_send)
        open_btn = QtWidgets.QPushButton("Open OpenFarm")
        open_btn.clicked.connect(lambda: self.safe(lambda: call("/local/show", {})))
        buttons = QtWidgets.QHBoxLayout()
        buttons.addWidget(open_btn)
        buttons.addStretch(1)
        buttons.addWidget(self.send_btn)
        layout = QtWidgets.QVBoxLayout(self)
        layout.addLayout(form)
        layout.addWidget(self.message)
        layout.addLayout(buttons)
        self.sender_obj = Sender()
        self.sender_obj.finished.connect(self.on_sent)
        self.refresh()

    def safe(self, fn):
        try:
            fn()
        except Exception as e:  # noqa: BLE001
            self.say(str(e), False)

    def say(self, text, ok=True):
        self.message.setText(text)
        self.message.setStyleSheet("" if ok else "color: #f0504a;")

    def refresh(self):
        self.name.setText(os.path.splitext(str(rt.maxFileName))[0] or "Untitled")
        engine = engine_of_scene()
        self.engine.setText(
            {"corona": "Corona", "vray": "V-Ray", "vray_gpu": "V-Ray GPU", "fstorm": "FStorm"}.get(
                engine, "%s (not supported)" % rt.classOf(rt.renderers.current)
            )
        )
        self.camera.clear()
        cams = scene_cameras()
        self.camera.addItems(cams)
        view = rt.viewport.getCamera()
        if view is not None and view.name in cams:
            self.camera.setCurrentText(view.name)
        self.frames.setText(scene_frames())
        folder, name, fmt = default_output()
        self.folder.setText(folder)
        self.file.setText(name)
        self.format.setCurrentText(fmt)
        self.send_btn.setEnabled(engine is not None and bool(cams))
        if not cams:
            self.say("The scene has no camera. Add one to render on the farm.", False)
        elif engine is None:
            self.say("OpenFarm renders V-Ray, V-Ray GPU, Corona and FStorm scenes.", False)
        else:
            self.say("")

    def on_send(self):
        folder = self.folder.text().strip()
        if not folder:
            return self.say("Choose an output folder.", False)
        if len(folder) > 1 and folder[1] == ":" and folder[0].upper() == "C":
            return self.say(
                "The output folder is on this computer's C: drive; other machines can't write there. Use a shared folder.",
                False,
            )
        original = os.path.join(str(rt.maxFilePath), str(rt.maxFileName)) if rt.maxFileName else ""
        file_name = str(rt.maxFileName) or "untitled.max"
        snapshot = os.path.join(tempfile.gettempdir(), "OpenFarm", uuid.uuid4().hex, file_name)
        os.makedirs(os.path.dirname(snapshot), exist_ok=True)
        # A copy of the scene as it is now; the open file keeps its name and unsaved-changes state.
        if not rt.saveMaxFile(snapshot, clearNeedSaveFlag=False, useNewFile=False, quiet=True):
            return self.say("3ds Max could not save a copy of the scene.", False)
        spec = {
            "name": self.name.text().strip() or file_name,
            "engine": engine_of_scene(),
            "frames": self.frames.text().strip(),
            "camera": self.camera.currentText(),
            "output": {
                "folder": folder,
                "name": self.file.text().strip() or "frame_####",
                "format": self.format.currentText(),
            },
            "priority": self.priority.value(),
            "chunk": self.chunk.value() or None,
            "settings": {},
            "host_version": str(rt.maxVersion()[7]) if len(rt.maxVersion()) > 7 else None,
            "submitted_by": "%s (%s)" % (os.environ.get("USERNAME", ""), os.environ.get("COMPUTERNAME", "")),
            "paused": self.paused.isChecked(),
        }
        self.send_btn.setEnabled(False)
        self.say("Sending the scene to the farm…")
        self.sender_obj.send(snapshot, file_name, original, spec)

    def on_sent(self, text, ok):
        self.send_btn.setEnabled(True)
        self.say(text, ok)


def show():
    global _window
    try:
        import qtmax

        parent = qtmax.GetQMaxMainWindow()
    except Exception:  # noqa: BLE001
        parent = None
    if _window is None:
        _window = Window(parent)
    else:
        _window.refresh()
    _window.show()
    _window.raise_()
    _window.activateWindow()
