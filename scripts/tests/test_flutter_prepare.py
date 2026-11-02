import sys
from pathlib import Path
import plistlib
import tempfile
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from prepare_flutter_probe import replace_exact, update_plist, prepare, SOURCE

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

    def test_mac_template_matches_declared_arm64_abi(self):
        with tempfile.TemporaryDirectory() as d:
            project = Path(d)/'project'; project.mkdir()
            (project/'.metadata').write_text('fixture')
            native = Path(d)/'library.a'; native.write_bytes(b'fixture-only')
            config = project/'macos/Runner/Configs/Release.xcconfig'
            config.parent.mkdir(parents=True); config.write_text('// fixture\n')
            pbx = project/'macos/Runner.xcodeproj/project.pbxproj'
            pbx.parent.mkdir(parents=True)
            pbx.write_text('MACOSX_DEPLOYMENT_TARGET = 12.0;\n'*3)
            entitlement = project/'macos/Runner/Release.entitlements'
            entitlement.write_bytes(plistlib.dumps({'com.apple.security.app-sandbox': True}))
            prepare(project, native, 'macos')
            self.assertIn('ARCHS = arm64\nEXCLUDED_ARCHS = x86_64\n', config.read_text())
            self.assertEqual(pbx.read_text().count('MACOSX_DEPLOYMENT_TARGET = 13.0;'), 3)
            self.assertTrue(plistlib.loads(entitlement.read_bytes())['com.apple.security.app-sandbox'])
    def test_ios_build_uses_exact_regression_tested_helper(self):
        with tempfile.TemporaryDirectory() as d:
            project = Path(d)/'project'; project.mkdir()
            (project/'.metadata').write_text('fixture')
            native = Path(d)/'library.a'; native.write_bytes(b'fixture-only')
            config = project/'ios/Flutter/Release.xcconfig'
            config.parent.mkdir(parents=True); config.write_text('// fixture\n')
            info = project/'ios/Runner/Info.plist'; info.parent.mkdir(parents=True)
            info.write_bytes(plistlib.dumps({'original': True}))
            prepare(project, native, 'ios')
            expected = (SOURCE/'platform/AppDelegate.swift').read_text()+'\n'+(SOURCE/'platform/DirectoryOperation.swift').read_text()
            self.assertEqual((project/'ios/Runner/AppDelegate.swift').read_text(), expected)
            self.assertEqual((project/'pubspec.lock').read_bytes(), (SOURCE/'pubspec.lock').read_bytes())

if __name__ == '__main__': unittest.main()
