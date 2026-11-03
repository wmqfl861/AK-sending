import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import unittest
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from run_flutter_validation import command_payload, require_arm64, require_ffi_exports, package_zip
from prepare_flutter_probe import SYMBOLS
from run_validation import run_one, ROOT


class FlutterEvidenceChecks(unittest.TestCase):
    def test_real_runner_header_is_not_architecture(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv = [sys.executable, '-c', "print('arm64')"]
            row = run_one('lipo-fixture', argv, root, root)
            self.assertEqual(row['exit_code'], 0)
            require_arm64(root / row['log'], argv)

    def test_architecture_exact_payload_with_lf_or_crlf(self):
        argv = ['lipo', '-archs', '/arm64/path/with space/app']
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory)/'architecture.log'
            for newline in ['\n', '\r\n']:
                log.write_bytes(('command='+json.dumps(argv)+newline+'arm64'+newline).encode())
                require_arm64(log, argv)
            for payload in ['', 'x86_64', 'arm64 x86_64', 'arm64\nwarning', 'arm64e', 'arm64\narm64']:
                log.write_text('command='+json.dumps(argv)+'\n'+payload, encoding='utf-8')
                with self.assertRaises(ValueError): require_arm64(log, argv)

    def test_malformed_or_wrong_command_header_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            log=Path(directory)/'log'
            for text in ['arm64\n', 'command=[]\narm64\n', 'command=invalid\narm64\n', 'command=["lipo"]']:
                log.write_text(text)
                with self.assertRaises(ValueError): command_payload(log, ['lipo','-archs','app'])

    def test_exact_defined_function_exports_not_strings_in_command_or_suffixes(self):
        with tempfile.TemporaryDirectory() as directory:
            log=Path(directory)/'symbols'
            argv=['nm','-gU',' '.join('_ak_m0_probe_'+s for s in SYMBOLS)]
            header='command='+json.dumps(argv)+'\n'
            body=''.join('00001234 T _ak_m0_probe_'+s+'\n' for s in SYMBOLS)
            log.write_text(header+body)
            require_ffi_exports(log, argv)
            for bad in ['',body.replace(' T ',' U '),body.replace('_start','_start_fake'),body.replace('00001234 T _ak_m0_probe_start\n','')]:
                log.write_text(header+bad)
                with self.assertRaises(ValueError): require_ffi_exports(log,argv)

    def test_sdk_install_package_pin_matches_workflow_without_downgrade(self):
        pins=json.loads((ROOT/'config/toolchains.json').read_text())['android']
        workflow=(ROOT/'.github/workflows/flutter-probe.yml').read_text()
        self.assertEqual(pins['compile_api'],37)
        self.assertEqual(pins['target_api'],37)
        self.assertEqual(pins['sdk_package'],'platforms;android-37.0')
        self.assertIn("'"+pins['sdk_package']+"'",workflow)
        self.assertNotIn("'platforms;android-37'",workflow)


class BundleArchiveChecks(unittest.TestCase):
    def test_files_directories_and_modes_preserved(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); product=root/'probe.app'; product.mkdir()
            (product/'empty').mkdir(); binary=product/'probe'; binary.write_bytes(b'synthetic')
            binary.chmod(0o755); output=root/'out.zip'; package_zip(product,output)
            with zipfile.ZipFile(output) as archive:
                self.assertEqual(archive.read('probe.app/probe'),b'synthetic')
                self.assertIn('probe.app/empty/',archive.namelist())
                if os.name!='nt': self.assertTrue(archive.getinfo('probe.app/probe').external_attr>>16 & stat.S_IXUSR)

    @unittest.skipIf(os.name=='nt', 'Apple framework symlinks tested on Unix hosts')
    def test_framework_directory_and_file_symlinks_preserved_without_duplication(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); product=root/'probe.app'; real=product/'Framework/Versions/A'
            real.mkdir(parents=True); (real/'lib').write_bytes(b'synthetic')
            (real.parent/'Current').symlink_to('A',target_is_directory=True)
            (product/'Framework/lib').symlink_to('Versions/Current/lib')
            output=root/'out.zip'; package_zip(product,output)
            with zipfile.ZipFile(output) as archive:
                for name,target in [('probe.app/Framework/Versions/Current',b'A'),('probe.app/Framework/lib',b'Versions/Current/lib')]:
                    self.assertEqual(archive.read(name),target)
                    self.assertTrue(stat.S_ISLNK(archive.getinfo(name).external_attr>>16))
                self.assertNotIn('probe.app/Framework/Versions/Current/lib',archive.namelist())

    @unittest.skipIf(os.name=='nt', 'Apple framework symlinks tested on Unix hosts')
    def test_external_symlink_is_not_packaged(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); product=root/'probe.app'; product.mkdir()
            (root/'outside').write_text('not artifact data');(product/'link').symlink_to('../outside')
            with self.assertRaises(ValueError): package_zip(product,root/'out.zip')


if __name__=='__main__': unittest.main()
