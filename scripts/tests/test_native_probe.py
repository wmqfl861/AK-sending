import sys
from pathlib import Path
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from run_native_validation import elf_has_16k_alignment

HEADER = 'Class: ELF64\nMachine: AArch64\n'
LOAD = 'LOAD 0x000000 0x000000 0x000000 0x123 0x123 R E 0x4000\n'

class NativeProbeChecks(unittest.TestCase):
    def test_16k_loads(self):
        self.assertEqual(elf_has_16k_alignment(HEADER + LOAD), (True, 1))
    def test_larger_power_of_two_alignment(self):
        self.assertEqual(elf_has_16k_alignment(HEADER + LOAD.replace('0x4000', '0x10000')), (True, 1))
    def test_no_loads_not_passed(self):
        self.assertEqual(elf_has_16k_alignment(HEADER), (False, 0))
    def test_wrong_machine_or_word_size_rejected(self):
        for header in ['', HEADER.replace('AArch64', 'x86-64'), HEADER.replace('ELF64', 'ELF32')]:
            self.assertFalse(elf_has_16k_alignment(header + LOAD)[0])
    def test_4k_or_non_power_of_two_rejected(self):
        for alignment in ['0x1000', '0x6000', '0x0']:
            self.assertFalse(elf_has_16k_alignment(HEADER + LOAD.replace('0x4000', alignment))[0])
    def test_offset_address_mismatch_rejected(self):
        self.assertFalse(elf_has_16k_alignment(HEADER + LOAD.replace('LOAD 0x000000', 'LOAD 0x001000'))[0])
    def test_malformed_input_fails_closed(self):
        for load in ['LOAD broken', 'LOAD 0x0 0x0 garbage', LOAD + 'LOAD incomplete']:
            self.assertFalse(elf_has_16k_alignment(HEADER + load)[0])

if __name__ == '__main__':
    unittest.main()
