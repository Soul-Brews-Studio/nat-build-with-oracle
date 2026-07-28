# dual-console — two independent fullscreen terminals, one per HDMI (Pi)

A text tty (fbcon) mirrors every connector, so two HDMI screens always show the SAME
console. To show **different** content per screen you need a compositor. This is the
lightest way: labwc (extended: HDMI-A-1 @ 0,0, HDMI-A-2 @ 1920,0) + one fullscreen
`foot` terminal pinned to each output via labwc windowRules.

Install (Raspberry Pi OS with labwc/lightdm):
```
mkdir -p ~/.config/labwc
cp autostart ~/.config/labwc/autostart   # chmod +x
cp rc.xml    ~/.config/labwc/rc.xml
# ensure the desktop autologins into labwc, then:
systemctl restart lightdm
```
- `autostart` launches two `foot` terminals with app-ids `term-left` / `term-right`
  (left runs htop; right is a shell — put anything: journalctl -f, an oracle mirror, …).
- `rc.xml` windowRules do `MoveToOutput` (HDMI-A-1 / HDMI-A-2) + `ToggleFullscreen`.
- Verify master-safely with `grim -o HDMI-A-1 out.png` (grim sees the Wayland compositor;
  it's blind to a bare-KMS app like flowengine — use debugfs for that).

Credit: the console-workspace idea + tty/DRM wisdom came from black-oracle
(black.local runs five cage-per-VT consoles). This adapts it to two SIMULTANEOUS outputs.
