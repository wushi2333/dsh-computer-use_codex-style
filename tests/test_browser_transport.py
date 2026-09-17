from __future__ import annotations

import json
import os
import tempfile
import threading
import time
import unittest
import uuid
from pathlib import Path

from unittest.mock import patch

from computer_use import extension_transport as t
from computer_use.browser_api import BrowserSurface
from computer_use.extension_hub import ExtensionHub


class OfficialIdentityTests(unittest.TestCase):
    def test_pipe_and_host_identity_constants(self) -> None:
        self.assertEqual(t.PIPE_NAME_WIN32, "\\\\.\\pipe\\codex-browser-use")
        self.assertEqual(t.PIPE_NAME_POSIX, "/tmp/codex-browser-use")
        self.assertEqual(t.EXTENSION_HOST_NAME, "com.openai.codexextension")
        self.assertEqual(t.WINDOWS_MANIFEST_DIRECTORY, "AppData/Local/OpenAI/extension")
        self.assertEqual(
            t.WINDOWS_NATIVE_MESSAGING_REGISTRY_ROOT,
            "HKCU\\Software\\Google\\Chrome\\NativeMessagingHosts",
        )
        self.assertIn("hehggadaopoacecdllhhajmbjkdcmajg", t.EXTENSION_IDS)
        self.assertIn("odlomjlbamekndcpllcnffbgeohgkmjh", t.EXTENSION_IDS)

    def test_allowed_origins_are_the_official_shape(self) -> None:
        self.assertEqual(
            t.allowed_origins(("abc",)),
            ["chrome-extension://abc/"],
        )

    def test_pipe_name_env_override(self) -> None:
        previous = os.environ.get(t.ENV_PIPE_NAME)
        os.environ[t.ENV_PIPE_NAME] = "\\\\.\\pipe\\dsh-test-override"
        try:
            self.assertEqual(t.pipe_name(), "\\\\.\\pipe\\dsh-test-override")
        finally:
            if previous is None:
                os.environ.pop(t.ENV_PIPE_NAME, None)
            else:
                os.environ[t.ENV_PIPE_NAME] = previous
        self.assertEqual(t.pipe_name(platform="win32"), t.PIPE_NAME_WIN32)
        self.assertEqual(t.pipe_name(platform="linux"), t.PIPE_NAME_POSIX)


class FramingTests(unittest.TestCase):
    def test_frame_is_4_byte_le_length_plus_json(self) -> None:
        frame = t.encode_native_frame({"a": 1})
        self.assertEqual(frame, b'\x07\x00\x00\x00{"a":1}')
        message, rest = t.decode_native_frame(frame)
        self.assertEqual(message, {"a": 1})
        self.assertEqual(rest, b"")

    def test_incremental_decode(self) -> None:
        frame = t.encode_native_frame({"hello": True})
        self.assertEqual(t.decode_native_frame(frame[:2]), (None, frame[:2]))
        message, rest = t.decode_native_frame(frame + b"tail")
        self.assertEqual(message, {"hello": True})
        self.assertEqual(rest, b"tail")

    def test_rejects_frames_over_the_host_limit(self) -> None:
        with self.assertRaises(ValueError):
            t.decode_native_frame(b"\xff\xff\xff\xff" + b"x" * 8)

    def test_rejects_invalid_json(self) -> None:
        blob = b"{not json"
        with self.assertRaises(ValueError):
            t.decode_native_frame(len(blob).to_bytes(4, "little") + blob)


class ManifestTests(unittest.TestCase):
    def test_manifest_document_is_stdio_with_official_origins(self) -> None:
        document = t.manifest_document("C:/host/host.exe")
        self.assertEqual(document["type"], "stdio")
        self.assertEqual(document["name"], t.EXTENSION_HOST_NAME)
        self.assertEqual(document["allowed_origins"], t.allowed_origins())

    def test_manifest_path_uses_the_official_directory(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = t.manifest_path(home=tmp, platform="win32")
            self.assertEqual(
                path,
                Path(tmp) / "AppData" / "Local" / "OpenAI" / "extension"
                / "com.openai.codexextension.json",
            )

    def test_diagnose_reports_the_official_problems(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            missing = t.diagnose_manifest(None, home=tmp, platform="win32")
            self.assertTrue(missing["problem"].startswith("Native host manifest does not exist: "))
            mismatch = t.diagnose_manifest(
                {"name": "org.example.other", "allowed_origins": []},
                home=tmp,
                platform="win32",
            )
            self.assertIn("manifest name does not match com.openai.codexextension", mismatch["problem"])
            self.assertIn("allowed_origins does not include", mismatch["problem"])
            correct = t.diagnose_manifest(
                {"name": t.EXTENSION_HOST_NAME, "allowed_origins": t.allowed_origins()},
                home=tmp,
                platform="win32",
            )
            self.assertTrue(correct["correct"])
            self.assertIsNone(correct["problem"])

    def test_diagnose_rejects_an_unsupported_platform(self) -> None:
        report = t.diagnose_manifest({}, platform="plan9")
        self.assertFalse(report["correct"])
        self.assertIn("Unsupported platform", report["problem"])

    def test_registry_key_missing_is_reported(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            report = t.diagnose_manifest(
                None, home=tmp, platform="win32", registry_key_exists=False
            )
            self.assertIn("Windows native host registry key does not exist:", report["problem"])


class TransportSelectionTests(unittest.TestCase):
    def test_resolve_transport_defaults_to_the_current_transport(self) -> None:
        self.assertEqual(t.resolve_transport("pipe"), t.TRANSPORT_PIPE)
        self.assertEqual(t.resolve_transport("http"), t.TRANSPORT_HTTP)
        self.assertEqual(t.resolve_transport("nonsense"), t.DEFAULT_TRANSPORT)
        # The official family is available, but the shipped default is not flipped
        # until the official payload envelope is known.
        self.assertIn(t.TRANSPORT_PIPE, t.TRANSPORTS)

    def test_hub_follows_the_selected_transport(self) -> None:
        self.assertEqual(ExtensionHub().transport, t.resolve_transport())
        self.assertEqual(ExtensionHub(transport="pipe").transport, t.TRANSPORT_PIPE)
        self.assertEqual(ExtensionHub().endpoint(), f"http://127.0.0.1:{ExtensionHub().port}")

    def test_documented_gap_is_present(self) -> None:
        doc = t.__doc__ or ""
        self.assertIn("not in the package", doc)
        self.assertIn("payload", doc.lower())

    def test_hub_diagnose_includes_the_manifest_layer(self) -> None:
        report = ExtensionHub().diagnose()
        self.assertFalse(report["connected"])
        self.assertEqual(report["transport"], t.resolve_transport())
        self.assertIn("manifestPath", report)
        self.assertIn("Cannot communicate with the ChatGPT browser extension", report["message"])


@unittest.skipUnless(os.name == "nt", "named pipe transport is Windows-only")
class NamedPipeRoundTripTests(unittest.TestCase):
    def test_listener_and_client_round_trip_native_frames(self) -> None:
        name = "\\\\.\\pipe\\dsh-cu-test-" + uuid.uuid4().hex
        received: list[dict] = []
        listener = t.PipeListener(
            name=name,
            on_message=received.append,
            outbox=lambda: [{"type": "commands", "commands": ["ping"]}],
        )
        self.assertTrue(listener.start())
        client = t.PipeClient(name=name)
        connected = False
        deadline = time.time() + 5.0
        while time.time() < deadline:
            if client.connect():
                connected = True
                break
            time.sleep(0.05)
        try:
            self.assertTrue(connected, "could not connect to the extension pipe")
            self.assertTrue(client.send({"type": "hello", "instanceId": "inst-1"}))
            replies: list[dict] = []
            deadline = time.time() + 5.0
            while time.time() < deadline and not replies:
                replies = [reply for reply in client.poll(timeout=0.2) if isinstance(reply, dict)]
            self.assertIn({"type": "commands", "commands": ["ping"]}, replies)
            self.assertEqual(received and received[0].get("type"), "hello")
            self.assertEqual(received and received[0].get("instanceId"), "inst-1")
        finally:
            client.close()
            listener.stop()


class ExtensionHubStartupTests(unittest.TestCase):
    def test_hub_starts_by_default_when_env_unset(self) -> None:
        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop("COMPUTER_USE_EXTENSION", None)
            os.environ.pop("COMPUTER_USE_EXTENSION_PORT", None)
            with patch.object(ExtensionHub, "start") as mock_start:
                surface = BrowserSurface()
                mock_start.assert_called_once_with(8765)

    def test_hub_does_not_start_when_opted_out(self) -> None:
        for opt_out in ("0", "false", "no", "False", "NO", "0 "):
            with self.subTest(val=opt_out):
                with patch.dict(os.environ, {"COMPUTER_USE_EXTENSION": opt_out}, clear=False):
                    with patch.object(ExtensionHub, "start") as mock_start:
                        surface = BrowserSurface()
                        mock_start.assert_not_called()

    def test_hub_starts_when_opted_in(self) -> None:
        for opt_in in ("1", "true", "yes", "True", "YES"):
            with self.subTest(val=opt_in):
                with patch.dict(os.environ, {"COMPUTER_USE_EXTENSION": opt_in}, clear=False):
                    os.environ.pop("COMPUTER_USE_EXTENSION_PORT", None)
                    with patch.object(ExtensionHub, "start") as mock_start:
                        surface = BrowserSurface()
                        mock_start.assert_called_once_with(8765)

    def test_hub_respects_custom_port(self) -> None:
        with patch.dict(os.environ, {"COMPUTER_USE_EXTENSION_PORT": "9876"}, clear=False):
            os.environ.pop("COMPUTER_USE_EXTENSION", None)
            with patch.object(ExtensionHub, "start") as mock_start:
                surface = BrowserSurface()
                mock_start.assert_called_once_with(9876)

    def test_hub_start_oserror_silently_ignored(self) -> None:
        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop("COMPUTER_USE_EXTENSION", None)
            with patch.object(ExtensionHub, "start", side_effect=OSError("Address already in use")):
                surface = BrowserSurface()
                self.assertIsNotNone(surface.hub)


if __name__ == "__main__":
    unittest.main()
