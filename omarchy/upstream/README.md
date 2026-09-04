# No Omarchy patches are carried

This directory is intentionally empty. All previously prepared patches
were withdrawn after their underlying behavior was implemented in
chonkstep:

- logout uses the uwsm session and the compositor's clean termination
  path;
- night light uses `hyprland-ctm-control-v1`, so the real `hyprsunset`
  and Omarchy status/indicator path work unchanged;
- shell supervision, launch-or-focus and restart-shell use chonkstep's
  Hyprland-compatible IPC.

The project does not modify `/usr/share/omarchy`, Omarchy's scripts, or
the user's `~/.config/omarchy` tree.
