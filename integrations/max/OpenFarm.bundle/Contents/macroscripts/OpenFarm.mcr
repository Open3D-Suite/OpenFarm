-- SPDX-License-Identifier: GPL-3.0-or-later
-- OpenFarm
-- Customize User Interface > Group: Main UI > Category: OpenFarm > Send to OpenFarm
global openfarm_home

macroScript OpenFarmSubmit
    category:"OpenFarm"
    internalCategory:"OpenFarm"
    buttonText:"Send to OpenFarm"
    toolTip:"Send this scene to the OpenFarm render farm"
(
    on execute do
    (
        if (not (isKindOf openfarm_home String) or not (doesFileExist (openfarm_home + "openfarm_max.py"))) then
            messageBox "OpenFarm's 3ds Max button is not installed properly. Install it again from OpenFarm > This machine." title:"OpenFarm"
        else
            -- One fixed text: Python reads the folder by itself. Written into the text, a folder breaks it
            -- (it ends in a backslash and may hold a quote).
            python.Execute "import os\nimport sys\nimport pymxs\nd = os.path.normpath(pymxs.runtime.openfarm_home)\nif d not in sys.path: sys.path.insert(0, d)\nimport openfarm_max\nopenfarm_max.show()"
    )
)
