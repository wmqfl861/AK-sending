import sys
from pathlib import Path
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from lan_probe import ipv4, decode_offer, LIMIT

class LanOfferTests(unittest.TestCase):
    def test_private_and_exact_loopback(self):
        for ip in ['10.1.2.3', '172.16.0.1', '172.31.1.1', '192.168.1.1', '127.0.0.1']:
            self.assertIsInstance(ipv4(ip), int)
        self.assertEqual(ipv4('127.0.0.1'), 0x7f000001)
    def test_public_unspecified_multicast_mapped_and_dns_rejected(self):
        for ip in ['0.0.0.0','8.8.8.8','224.0.0.1','255.255.255.255','::ffff:192.168.1.1',
                   '169.254.1.1','localhost','127.0.0.2','172.32.1.1']:
            with self.assertRaises(ValueError): ipv4(ip)
    def test_offer_exact_and_bounded(self):
        self.assertEqual(decode_offer('AKL1|192.168.1.2|42424|00ff'), ('192.168.1.2',42424,b'\x00\xff'))
        self.assertEqual(len(decode_offer('AKL1|10.0.0.2|1024|' + 'AA'*LIMIT)[2]), LIMIT)
    def test_bad_offer_never_silently_truncated(self):
        for value in ['AKL2|10.0.0.2|42424|00', 'AKL1|8.8.8.8|42424|00',
                      'AKL1|10.0.0.2|0|00','AKL1|10.0.0.2|65536|00','AKL1|10.0.0.2|+42424|00',
                      'AKL1|10.0.0.2|٤٢٤٢٤|00','AKL1|10.0.0.2|42424|',
                      'AKL1|10.0.0.2|42424|0','AKL1|10.0.0.2|42424|xx',
                      'AKL1|10.0.0.2|42424|00 11','AKL1|10.0.0.2|42424|00|extra',
                      'AKL1|10.0.0.2|42424|'+'00'*(LIMIT+1)]:
            with self.assertRaises(ValueError): decode_offer(value)
if __name__ == '__main__': unittest.main()
