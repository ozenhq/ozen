# dmgbuild layout for the release DMG (.github/workflows/release.yml): Ozen.app and an Applications link on the
# background dmg.swift draws. Usage: dmgbuild -s dmg_settings.py -D app=<Ozen.app> -D bg=<bg.tiff> Ozen <out.dmg>
import os

app = defines["app"]  # noqa: F821 (dmgbuild injects defines)
files = [app]
symlinks = {"Applications": "/Applications"}
icon = os.path.join(app, "Contents/Resources/AppIcon.icns")  # the mounted volume's icon
background = defines["bg"]  # noqa: F821
window_rect = ((200, 120), (640, 430))  # dmg.swift draws 640x400; the extra 30 is the title bar
icon_size = 112
text_size = 13
icon_locations = {"Ozen.app": (170, 210), "Applications": (470, 210)}
show_status_bar = show_tab_view = show_toolbar = show_pathbar = show_sidebar = False
default_view = "icon-view"
format = "UDZO"
