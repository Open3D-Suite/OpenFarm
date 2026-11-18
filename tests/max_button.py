# SPDX-License-Identifier: GPL-3.0-or-later
"""Checks the 3ds Max button the way a person uses it, in a 3ds Max batch session (never in an artist's
Max). It needs no farm and sends nothing:

    3dsmaxbatch.exe tests/max_button.py -mxsString bundle:<.../integrations/max/OpenFarm.bundle>

Write the path with forward slashes: 3ds Max reads a backslash in it as the start of an escape.

1. The button. Copies the bundle into a folder whose name has a space, an apostrophe and an accented
   letter, loads its start script and its macro from there, runs the "Send to OpenFarm" macro and expects
   the window open.
2. The unsaved changes mark. Marks a saved scene as changed and runs what "Send to the farm" runs, with
   the upload replaced by a note of what it was given. Expects the mark still set and the scene's file
   name untouched, also when the send fails after the copy was saved; a scene with no changes stays so.

Prints a line per check and "RESULT: PASS" or "RESULT: FAIL" (see them with -listenerlog <file>); a
failure also ends 3dsmaxbatch with an error.
"""

import os
import shutil
import sys
import tempfile

from pymxs import runtime as rt

failed = []


def check(what, ok, detail=""):
    print("PASS" if ok else "FAIL", what, ascii(detail) if detail else "")
    if not ok:
        failed.append(what)


def the_button(home):
    scripts = os.path.join(home, "Contents", "scripts")
    rt.fileIn(os.path.join(scripts, "OpenFarm_bundle.ms"))
    check("the bundle knows its folder", str(rt.openfarm_home) == scripts + os.sep, str(rt.openfarm_home))
    # macros.load reads the macro where it is; with fileIn, 3ds Max would keep a copy of it in the user's
    # own macro folder.
    rt.macros.load(os.path.join(home, "Contents", "macroscripts"))
    # An error inside the macro does not reach this script: 3ds Max writes it in the listener.
    rt.macros.run("OpenFarm", "OpenFarmSubmit")
    module = sys.modules.get("openfarm_max")
    loaded = module is not None and os.path.samefile(os.path.dirname(module.__file__), scripts)
    check("the macro loaded the submitter of its own bundle", loaded)
    check("the macro opened the window", loaded and module._window is not None and module._window.isVisible())
    return module if loaded else None


def the_unsaved_mark(ofm, work):
    window = ofm._window or ofm.Window()
    rt.resetMaxFile(rt.Name("noPrompt"))
    rt.Box()
    rt.saveMaxFile(os.path.join(work, "button test.max"), quiet=True)
    name = (str(rt.maxFilePath), str(rt.maxFileName))
    window.folder.setText(r"\\server\share\renders")
    sent = []

    def note(snapshot, file_name, original, spec):
        sent.append((os.path.getsize(snapshot), file_name, original))
        os.remove(snapshot)
        os.rmdir(os.path.dirname(snapshot))

    def fail(snapshot, file_name, original, spec):
        note(snapshot, file_name, original, spec)
        raise RuntimeError("this send fails after the copy was saved")

    def mark_after_a_send(mark, send):
        window.sender_obj.send = send
        rt.setSaveRequired(mark)
        try:
            window.on_send()
        except RuntimeError as e:
            print("The send stopped:", e)
        rt.windows.processPostedMessages()
        return bool(rt.getSaveRequired())

    check("a changed scene is still marked as changed after a send", mark_after_a_send(True, note) is True)
    check("a scene with no changes still has none after a send", mark_after_a_send(False, note) is False)
    check("a changed scene is still marked as changed after a send that fails", mark_after_a_send(True, fail) is True)
    copies = [s for s in sent if s[0] > 0 and s[1] == name[1] and s[2] == os.path.join(*name)]
    check("each send was given a copy of the scene", len(sent) == 3 and len(copies) == 3, sent)
    check("the scene keeps its file name", (str(rt.maxFilePath), str(rt.maxFileName)) == name, str(rt.maxFileName))
    window.close()


bundle = str(rt.maxOps.mxsCmdLineArgs[rt.Name("bundle")])
work = tempfile.mkdtemp(prefix="openfarm-max-button-")
macros = str(rt.getDir(rt.Name("userMacros")))
macros_before = sorted(os.listdir(macros))
try:
    home = os.path.join(work, "O'Brien y Peña", "OpenFarm.bundle")
    shutil.copytree(bundle, home)
    ofm = the_button(home)
    if ofm is None:
        print("The macro did not load the submitter: it is loaded here to check the unsaved changes mark.")
        sys.path.insert(0, os.path.join(home, "Contents", "scripts"))
        import openfarm_max as ofm
    the_unsaved_mark(ofm, work)
    check("3ds Max's own macro folder is as it was", sorted(os.listdir(macros)) == macros_before)
finally:
    rt.resetMaxFile(rt.Name("noPrompt"))
    shutil.rmtree(work, ignore_errors=True)
print("RESULT:", "FAIL (%d): %s" % (len(failed), "; ".join(failed)) if failed else "PASS")
if failed:
    raise RuntimeError("max_button: %d checks failed" % len(failed))
