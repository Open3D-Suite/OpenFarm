# SPDX-License-Identifier: GPL-3.0-or-later
"""Checks that every text of the Blender add-on has its Spanish in SPANISH, without Blender:

    python -I tests/blender_texts.py

It reads integrations/blender/openfarm/__init__.py as text (ast) and gathers what the add-on shows: the
name and description of each property and of each choice of a list, the bl_label and bl_description of
each class, and the texts given to iface_, rpt_, _set_status and RuntimeError. Each one must be a key of
SPANISH, or a name that stays as it is (KEPT); each key of SPANISH must be one of them; and a Spanish text
must keep the %s and #### of its English one. Prints what is wrong and ends with an error if anything is.
"""

import ast
import sys
from pathlib import Path

ADD_ON = Path(__file__).resolve().parent.parent / "integrations" / "blender" / "openfarm" / "__init__.py"
# Names of the add-on, of file formats, of devices: the same in every language.
KEPT = {"OpenFarm", "OpenEXR", "PNG", "JPEG", "OptiX", "CUDA", "HIP", "oneAPI", "CPU"}
# Calls whose first argument is a text the person sees.
SHOWN_BY = {"iface_", "rpt_", "_set_status", "RuntimeError"}
PLACEHOLDERS = ("%s", "####")


def text_of(node):
    return node.value if isinstance(node, ast.Constant) and isinstance(node.value, str) else None


def name_of(func):
    if isinstance(func, ast.Name):
        return func.id
    if isinstance(func, ast.Attribute):
        return func.attr
    return None


def shown_texts(tree):
    """The texts the add-on shows, with the line of each."""
    found = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Call):
            called = name_of(node.func)
            if called in SHOWN_BY and node.args and text_of(node.args[0]) is not None:
                found.append((text_of(node.args[0]), node.lineno))
            if called and called.endswith("Property"):
                for keyword in node.keywords:
                    if keyword.arg in ("name", "description") and text_of(keyword.value):
                        found.append((text_of(keyword.value), keyword.value.lineno))
                    if keyword.arg == "items" and isinstance(keyword.value, (ast.List, ast.Tuple)):
                        for item in keyword.value.elts:
                            for part in item.elts[1:] if isinstance(item, ast.Tuple) else []:
                                if text_of(part):
                                    found.append((text_of(part), part.lineno))
        if isinstance(node, ast.ClassDef):
            for statement in node.body:
                if isinstance(statement, ast.Assign) and text_of(statement.value) is not None:
                    if any(
                        isinstance(t, ast.Name) and t.id in ("bl_label", "bl_description") for t in statement.targets
                    ):
                        found.append((text_of(statement.value), statement.lineno))
    return found


def spanish_of(tree):
    """SPANISH, as {English: Spanish}."""
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "SPANISH" for t in node.targets):
            return ast.literal_eval(node.value)
    return None


def problems(source):
    tree = ast.parse(source)
    spanish = spanish_of(tree)
    if spanish is None:
        return ["there is no SPANISH table"]
    wrong = []
    shown = shown_texts(tree)
    for text, line in shown:
        if text not in spanish and text not in KEPT:
            wrong.append("line %d: no Spanish for %r" % (line, text))
    used = {text for text, _ in shown}
    for english, text in spanish.items():
        if english not in used:
            wrong.append("SPANISH has %r, which the add-on does not show" % english)
        if not text.strip() or text == english:
            wrong.append("the Spanish of %r is empty or the same text" % english)
        for mark in PLACEHOLDERS:
            if english.count(mark) != text.count(mark):
                wrong.append("the Spanish of %r does not keep its %s" % (english, mark))
    return wrong


def main():
    wrong = problems(ADD_ON.read_text(encoding="utf-8"))
    # The check itself: a text with no Spanish and an extra key must both be found.
    planted = problems('SPANISH = {"Unused": "Sin usar"}\nclass A:\n    bl_label = "New text"\n')
    if not any("New text" in line for line in planted) or not any("Unused" in line for line in planted):
        wrong.append("the check does not find a missing or an extra text: %r" % planted)
    for line in wrong:
        print(line)
    print("RESULT: %s" % ("FAIL" if wrong else "PASS"))
    return 1 if wrong else 0


if __name__ == "__main__":
    sys.exit(main())
