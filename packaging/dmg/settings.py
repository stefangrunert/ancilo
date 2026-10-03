# The DMG window, for dmgbuild (packaging/dmg.sh): Ancilo on the left, the
# Applications folder on the right, the background between them (drawn by
# packaging/dmg/background.swift). Nothing else in sight – the volume icon
# and the background folder lie outside the window, even when hidden files
# are shown.
import os.path

app = defines["app"]  # noqa: F821 – given by dmg.sh
name = os.path.basename(app)

format = "UDZO"
compression_level = 9
filesystem = "HFS+"
files = [app]
symlinks = {"Applications": "/Applications"}
icon = defines["volicon"]  # noqa: F821
badge_icon = None
hide_extensions = [name]

background = defines["background"]  # noqa: F821
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
sidebar_width = 0
window_rect = ((200, 120), (660, 400))
default_view = "icon-view"
show_icon_preview = False
arrange_by = None
icon_size = 128
text_size = 13
label_pos = "bottom"
icon_locations = {
    name: (170, 200),
    "Applications": (490, 200),
    # Out of sight, also with hidden files shown.
    ".background.tiff": (170, 800),
    ".VolumeIcon.icns": (490, 800),
    ".fseventsd": (330, 800),
    ".Trashes": (330, 900),
}
