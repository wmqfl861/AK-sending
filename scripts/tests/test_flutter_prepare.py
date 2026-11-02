import sys
from pathlib import Path
import plistlib
import tempfile
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from prepare_flutter_probe import replace_exact, update_plist, prepare

class FlutterPreparation(unittest.TestCase):
    def test_exact_template_change(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d)/'template'; p.write_text('old\n')
            replace_exact(p, 'old', 'new')
            self.assertEqual(p.read_text(), 'new\n')
    def test_missing_or_duplicate_anchor_fails_without_edit(self):
        for text in ['unrelated', 'old old']:
            with tempfile.TemporaryDirectory() as d:
                p=Path(d)/'template'; p.write_text(text)
                with self.assertRaises(ValueError): replace_exact(p, 'old', 'new')
                self.assertEqual(p.read_text(), text)
    def test_plist_update_retains_existing_values(self):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/'Info.plist';p.write_bytes(plistlib.dumps({'old':True}))
            update_plist(p, {'new':'value'})
            self.assertEqual(plistlib.loads(p.read_bytes()), {'old':True,'new':'value'})
    def test_missing_generated_project_or_library_is_not_success(self):
        with tempfile.TemporaryDirectory() as d:
            with self.assertRaises(ValueError): prepare(Path(d),Path(d)/'missing','linux')

if __name__ == '__main__': unittest.main()
