#!/usr/bin/env python3
"""GTK3 fixture for the wait_for tests: a window whose accessibility tree changes on cue.

The app publishes one label whose text moves through a known sequence: it starts at the
WAITING sentinel and becomes READY only when the file named by the first argv is created.
That gives the test a change it controls exactly, rather than one it has to race.
"""
import os, sys, threading, time

import gi
gi.require_version('Gtk', '3.0')
from gi.repository import Gtk, GLib

trigger = sys.argv[1]

window = Gtk.Window(title='waitfor-fixture')
window.set_default_size(320, 200)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
label = Gtk.Label(label='WAITING')
box.pack_start(label, True, True, 0)
button = Gtk.Button(label='Confirm')
box.pack_start(button, False, False, 0)
window.add(box)
window.show_all()

done = {'flipped': False}


def watch():
    # Poll the trigger file rather than using Gtk timeouts, so the change happens at a
    # moment the test chose and not on a timer that could fire mid-assertion.
    while not done['flipped']:
        if os.path.exists(trigger):
            def flip():
                label.set_text('READY')
            GLib.idle_add(flip)
            done['flipped'] = True
            return
        time.sleep(0.05)


threading.Thread(target=watch, daemon=True).start()
Gtk.main()
