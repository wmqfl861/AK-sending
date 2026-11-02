import importlib.util
import io
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    "console_runner", Path(__file__).resolve().parents[1] / "run_validation.py")
assert spec is not None and spec.loader is not None
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class ConsoleEncodingTests(unittest.TestCase):
    def test_cp1252_redirected_stdout_can_mirror_unicode_tree(self):
        # Exercise both newline conventions on every host; encoding is independent.
        for newline in ("\n", "\r\n"):
            with self.subTest(newline=repr(newline)):
                buffer = io.BytesIO()
                stream = io.TextIOWrapper(buffer, encoding="cp1252", newline=newline)
                try:
                    runner.configure_console(stream)
                    text = "\u251c\u2500\u2500 quinn \u76f4\u8fde\n"
                    print(text, end="", file=stream, flush=True)
                    expected = text.replace("\n", newline).encode("utf-8")
                    self.assertEqual(buffer.getvalue(), expected)
                finally:
                    stream.detach()

    def test_unicode_string_stream_needs_no_reconfiguration(self):
        stream = io.StringIO()
        runner.configure_console(stream)
        print("\u2514\u2500\u2500 rustls", file=stream)
        self.assertIn("rustls", stream.getvalue())
