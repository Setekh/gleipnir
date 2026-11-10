# The disk image's window, for dmgbuild: Gleipnir and an Applications shortcut side by side on
# dmg-background.png, to drag one onto the other. script/bundle passes the app and icon with -D.
import os.path

app = defines["app"]  # noqa: F821, dmgbuild provides it

format = "UDZO"
filesystem = "HFS+"
files = [app]
symlinks = {"Applications": "/Applications"}
icon = defines["volume_icon"]  # noqa: F821
hide_extensions = [os.path.basename(app)]

background = os.path.join(defines["settings_dir"], "dmg-background.png")  # noqa: F821
# The background is 660 by 400; Finder's title bar adds to the height.
window_rect = ((200, 140), (660, 428))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
show_icon_preview = False
icon_size = 128
text_size = 13
arrange_by = None
icon_locations = {
    os.path.basename(app): (165, 180),
    "Applications": (495, 180),
}
