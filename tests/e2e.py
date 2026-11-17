# SPDX-License-Identifier: GPL-3.0-or-later
"""End-to-end check against a running OpenFarm (any Python 3, standard library only).

    python -I tests/e2e.py <local app url> <output folder> [engine] [scene path] [frames]

With the test renderer (engine "dummy", the default) it needs nothing else: it submits 20 frames of which
frame 7 always fails, waits, and checks that the job ends as expected. With a real engine it snapshots the
scene through the server and renders the given frames.
"""

import json
import os
import sys
import time
import urllib.request


def call(base, path, body=None):
    data = None if body is None else json.dumps(body).encode()
    # The address is the one this test is given on its command line.
    req = urllib.request.Request(base + path, data=data, headers={"content-type": "application/json"})  # noqa: S310
    with urllib.request.urlopen(req, timeout=60) as r:  # noqa: S310
        return json.loads(r.read() or b"null")


def main():
    base, out = sys.argv[1].rstrip("/"), sys.argv[2]
    engine = sys.argv[3] if len(sys.argv) > 3 else "dummy"
    scene_path = sys.argv[4] if len(sys.argv) > 4 else ""
    frames = sys.argv[5] if len(sys.argv) > 5 else "1-20"
    camera = sys.argv[6] if len(sys.argv) > 6 else None
    if engine == "dummy":
        scene = {"hash": "0" * 64, "file_name": "test", "original_path": "", "size": 0}
        settings = {"secs": 0.6, "fail_frames": [7]}
        fmt = "png"
    else:
        scene = call(base, "/api/v1/scenes/from-path", {"path": scene_path})
        settings = {}
        fmt = "exr"
    spec = {
        "name": "e2e %s %s" % (engine, time.strftime("%H:%M:%S")),
        "engine": engine,
        "scene": scene,
        "frames": frames,
        "camera": camera,
        "output": {"folder": out, "name": engine + "_####", "format": fmt},
        "device": "OPTIX" if engine == "cycles" else None,
        "settings": settings,
        "submitted_by": "e2e",
    }
    job = call(base, "/api/v1/jobs", spec)["id"]
    print("job", job)
    t0 = time.time()
    while True:
        d = call(base, "/api/v1/jobs/%d" % job)
        s = d["summary"]
        print(
            "  %5.1f s  %-9s done %d  rendering %d  failed %d"
            % (time.time() - t0, s["state"], s["done"], s["rendering"], s["failed"])
        )
        if s["state"] in ("done", "failed", "canceled"):
            break
        time.sleep(1)
    machines = {}
    for f in d["frames"]:
        machines[f["machine"]] = machines.get(f["machine"], 0) + 1
    print("frames per machine:", machines)
    print("first tasks:", [t["frames"] for t in d["tasks"][:6]])
    print("tasks:", len(d["tasks"]), "failed tasks:", sum(t["state"] == "failed" for t in d["tasks"]))
    bad = [f for f in d["frames"] if f["state"] != "done"]
    print("not done:", [(f["frame"], f["state"], f["attempts"], f["warning"]) for f in bad])
    print("previews:", sum(f["thumb"] for f in d["frames"]), "files:", len(os.listdir(out)))
    if engine == "dummy":
        ok = s["state"] == "failed" and [f["frame"] for f in bad] == [7] and s["done"] == 19 and bad[0]["attempts"] == 3
    else:
        ok = s["state"] == "done"
    print("OK" if ok else "UNEXPECTED")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
