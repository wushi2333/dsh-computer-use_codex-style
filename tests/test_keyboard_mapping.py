from __future__ import annotations

import unittest

from computer_use.keys import virtual_keys
from computer_use.policy import deny_press_key


class KeyboardMappingTests(unittest.TestCase):
    def test_windows_key_variants_rejected_by_policy(self) -> None:
        """TC-16: Windows key and its aliases must be strictly rejected by policy."""
        denied_aliases = [
            "Win", "WINDOWS", "Meta", "Command", "Cmd", "Super", "OS",
            "Win_L", "WINDOWS_L", "Meta_L", "Command_L", "Cmd_L", "Super_L", "OS_L",
            "Win_R", "WINDOWS_R", "Meta_R", "Command_R", "Cmd_R", "Super_R", "OS_R",
            "lwin", "rwin"
        ]
        for alias in denied_aliases:
            with self.assertRaises(Exception, msg=f"{alias} should be rejected by deny_press_key"):
                deny_press_key(alias)

    def test_common_chords_resolve_cleanly(self) -> None:
        """Verify common application chords and control keys resolve without error."""
        chords = [
            ("Control_L+a", [0x11, 0x41]),
            ("Control_L+c", [0x11, 0x43]),
            ("Control_L+v", [0x11, 0x56]),
            ("Control_L+t", [0x11, 0x54]),
            ("Control_L+w", [0x11, 0x57]),
            ("Shift_L+Tab", [0x10, 0x09]),
            ("Alt_L+F4", [0x12, 0x73]),
        ]
        for chord_str, expected_vks in chords:
            vks = virtual_keys(chord_str)
            self.assertEqual(vks, expected_vks, f"Chord {chord_str} should resolve to {expected_vks}")

    def test_navigation_and_editing_keys(self) -> None:
        """Navigation, editing and function keys resolve to proper Virtual Key codes."""
        test_cases = [
            ("Return", 0x0D),
            ("Enter", 0x0D),
            ("Tab", 0x09),
            ("Escape", 0x1B),
            ("Esc", 0x1B),
            ("BackSpace", 0x08),
            ("Delete", 0x2E),
            ("Home", 0x24),
            ("End", 0x23),
            ("Page_Up", 0x21),
            ("Page_Down", 0x22),
            ("Up", 0x26),
            ("Down", 0x28),
            ("Left", 0x25),
            ("Right", 0x27),
            ("F1", 0x70),
            ("F5", 0x74),
            ("F12", 0x7B),
        ]
        for key_name, expected_vk in test_cases:
            vks = virtual_keys(key_name)
            self.assertEqual(vks, [expected_vk], f"{key_name} should resolve to 0x{expected_vk:X}")

    def test_punctuation_not_aliased_to_win(self) -> None:
        """Punctuation keys must never collide with Windows key VKs (0x5B, 0x5C)."""
        punctuations = ["[", "]", "\\", ";", "`", "~", "|", "{", "}", "_"]
        for p in punctuations:
            vks = virtual_keys(p)
            for vk in vks:
                self.assertNotIn(vk, (0x5B, 0x5C), f"Punctuation '{p}' mapped to Windows Key!")

    def test_press_chord_event_ordering(self) -> None:
        """Verify press_chord sends modifiers down in order and releases in reverse order."""
        from computer_use.win_input import press_chord, _send
        from unittest.mock import patch

        recorded_events = []
        with patch("computer_use.win_input._send", side_effect=lambda evs: recorded_events.extend(evs)):
            press_chord("Control_L+a")

        # Control_L down, A down, A up, Control_L up = 4 events
        self.assertEqual(len(recorded_events), 4)
        # Event 0: Control_L down (dwFlags == 0)
        self.assertEqual(recorded_events[0].union.ki.wVk, 0x11)
        self.assertEqual(recorded_events[0].union.ki.dwFlags, 0)
        # Event 1: 'A' down (dwFlags == 0)
        self.assertEqual(recorded_events[1].union.ki.wVk, 0x41)
        self.assertEqual(recorded_events[1].union.ki.dwFlags, 0)
        # Event 2: 'A' up (dwFlags == KEYEVENTF_KEYUP)
        self.assertEqual(recorded_events[2].union.ki.wVk, 0x41)
        self.assertEqual(recorded_events[2].union.ki.dwFlags, 2)
        # Event 3: Control_L up (dwFlags == KEYEVENTF_KEYUP)
        self.assertEqual(recorded_events[3].union.ki.wVk, 0x11)
        self.assertEqual(recorded_events[3].union.ki.dwFlags, 2)


if __name__ == "__main__":
    unittest.main()
