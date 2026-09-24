# The dmg window, for dmgbuild (https://dmgbuild.readthedocs.io):
#
#   dmgbuild -s scripts/dmg-settings.py -D app=DuckLocal.app DuckLocal out.dmg
#
# dmgbuild writes the window's layout (.DS_Store) itself, so this works on a
# CI runner with no Finder session to script. The background is drawn for
# these positions by scripts/make-icons.py; move an icon here and it moves
# there too.
import os.path

app = defines["app"]  # noqa: F821 (dmgbuild provides `defines`)
name = os.path.basename(app)

format = "UDZO"
files = [app]
symlinks = {"Applications": "/Applications"}

background = "assets/dmg-background.tiff"
window_rect = ((200, 120), (640, 320))
icon_size = 128
text_size = 13
icon_locations = {
    name: (170, 150),
    "Applications": (470, 150),
}

default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
show_icon_preview = False
arrange_by = None
