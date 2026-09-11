#!/usr/bin/env python3
"""Real GTK/terminal demo clients. All displayed content is a product fixture."""
import argparse
import math
import signal
import time


def terminal(role, version, scenario):
    cyan, green, gray, reset = '\033[96m', '\033[92m', '\033[90m', '\033[0m'
    print('\033[2J\033[H', end='')
    if scenario == 'spaces':
        lines = [f'{cyan}chonkstep {version}{reset}  /  Spaces', '',
                 '$ cat desktop-workflow.txt', '',
                 f'  {green}Control ↑{reset}    Open Overview',
                 f'  {green}Control ← →{reset}  Switch desktops',
                 f'  {green}Control ⌘ F{reset}  Fullscreen Space', '',
                 '  Desktop 1  /  Terminal workspace',
                 '  Desktop 2  /  Design studio',
                 '  Desktop 3  /  Release notes', '',
                 f'{gray}Real windows. Live desktop thumbnails.{reset}', '', '$ ']
    elif role == 'terminal':
        lines = [f'{cyan}chonkstep {version}{reset}  /  product demo', '',
                 '$ cat capture-workflow.txt', '',
                 f'  {green}⌘ ⇧ 3{reset}   Capture the desktop',
                 f'  {green}⌘ ⇧ 4{reset}   Select a region',
                 f'  {green}⌘ ⇧ 5{reset}   Open capture controls', '',
                 '  Screen · Window · Area · Record', '',
                 f'{gray}Scripted workspace · real compositor{reset}', '', '$ ']
    else:
        lines = [f'{cyan}~/studio / launch-notes{reset}', '', '$ cat checklist.md', '',
                 '  [x] Set up the desktop', '  [x] Open the capture overlay',
                 '  [ ] Select a window or region', '  [ ] Save a screenshot',
                 '  [ ] Record a short clip', '',
                 f'{gray}Demo content; these are not benchmark results.{reset}', '', '$ ']
    print('\r\n'.join(lines), end='', flush=True)
    while True:
        signal.pause()


def editor():
    import gi
    gi.require_version('Gtk', '4.0')
    from gi.repository import Gtk
    app = Gtk.Application(application_id='org.chonkstep.DemoNotes')
    def activate(app):
        window = Gtk.ApplicationWindow(application=app, title='Studio / Release notes')
        window.set_titlebar(Gtk.HeaderBar())
        window.set_default_size(810, 560)
        text = Gtk.TextView(monospace=True, editable=True,
            top_margin=35, bottom_margin=35, left_margin=35, right_margin=35)
        text.get_buffer().set_text('CHONKSTEP / FIELD NOTES\n\n'
            'A place for every project.\n\n'
            '01  Keep terminals together.\n'
            '02  Give the design board room to breathe.\n'
            '03  Write the release notes here.\n\n'
            'Open Overview to see every desktop.\n'
            'Drag a window to move it to another Space.\n'
            'Fullscreen creates a temporary Space.\n\n'
            'Your windows stay where you put them.')
        css = Gtk.CssProvider()
        css.load_from_data(b'textview { font-size: 19px; } textview text { background: #172b32; color: #d1e8e3; }')
        Gtk.StyleContext.add_provider_for_display(window.get_display(), css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
        window.set_child(text)
        window.present()
    app.connect('activate', activate)
    app.run([])


def board():
    import gi
    gi.require_version('Gtk', '4.0')
    from gi.repository import Gtk, GLib
    app = Gtk.Application(application_id='org.chonkstep.DemoBoard')
    started = time.monotonic()

    def activate(app):
        window = Gtk.ApplicationWindow(application=app, title='Studio / Capture a little inspiration')
        window.set_titlebar(Gtk.HeaderBar())
        window.set_default_size(780, 590)
        canvas = Gtk.DrawingArea()

        def paint(_area, cr, w, h):
            def text(x, y, value, size, color=(.87, .90, .96)):
                cr.set_source_rgb(*color)
                cr.select_font_face('sans-serif', 0, 0)
                cr.set_font_size(size)
                cr.move_to(x, y)
                cr.show_text(value)
            cr.set_source_rgb(.075, .09, .14)
            cr.paint()
            text(34, 48, 'STUDIO   /   CHONKSTEP', 15, (.50, .69, .94))
            text(34, 100, 'A desktop worth capturing.', 30)
            text(35, 135, 'Select it. Save it. Share it.', 18, (.62, .68, .78))
            x, y, bw, bh = 34, 171, w-68, h-274
            cr.set_source_rgb(.13, .20, .31)
            cr.rectangle(x, y, bw, bh)
            cr.fill()
            for i, color in enumerate([(.22,.43,.67), (.30,.64,.74), (.74,.79,.64)]):
                cr.set_source_rgb(*color)
                cr.move_to(x, y+bh)
                for step in range(101):
                    px = step / 100
                    py = .53 + .18*math.sin(px*6+i*.9) + i*.12
                    cr.line_to(x+px*bw, y+bh*(1-py))
                cr.line_to(x+bw, y+bh)
                cr.close_path()
                cr.fill()
            cr.set_source_rgb(.95,.81,.53)
            cr.arc(x+bw*.76, y+bh*.25, 29, 0, 2*math.pi)
            cr.fill()
            text(35, h-64, 'FIELD NOTES  /  001', 14, (.50,.69,.94))
            text(35, h-33, 'A small collection of shapes, light and open windows.', 16)
            # Real live animation makes the saved region recording verifiable.
            cr.set_source_rgb(.50,.81,.76)
            cr.arc(w-46, 44, 5+2*math.sin(time.monotonic()-started), 0, 2*math.pi)
            cr.fill()
        canvas.set_draw_func(paint)
        window.set_child(canvas)
        window.present()
        GLib.timeout_add(33, lambda: (canvas.queue_draw(), True)[1])
    app.connect('activate', activate)
    app.run([])


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('role', choices=['terminal', 'notes', 'board', 'editor'])
    parser.add_argument('--version', default='preview')
    parser.add_argument('--scenario', choices=['capture', 'spaces'], default='capture')
    args = parser.parse_args()
    if args.role == 'board':
        board()
    elif args.role == 'editor':
        editor()
    else:
        terminal(args.role, args.version, args.scenario)
