import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
from validate_device_evidence import CASES, digest, evidence_file, unique_object, validate

SHA = 'a' * 40
class DeviceEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root/'app.bin').write_bytes(b'synthetic artifact')
        (self.root/'log.txt').write_bytes(b'synthetic evidence, NOT a real physical test')
        self.ref = {'path':'log.txt','sha256':digest(self.root/'log.txt')}
        self.doc = {'schema':1,'commit':SHA,'build':'release',
          'device':{'kind':'physical','platform':'android','model':'fixture','os_build':'fixture','coverage':'current'},
          'artifact':{'path':'app.bin','sha256':digest(self.root/'app.bin')},
          'cases':{case:{'status':'PASS','observed_at':'2026-11-02T12:00:00-08:00','evidence':dict(self.ref)} for case in CASES},
          'metrics':{'memory_metric':'PSS','idle_memory_bytes':10,'installed_bytes':10,'idle_cpu_percent':0.1,
                     'idle_sample_seconds':600,'cold_start_ms':[100]*10}}
    def test_complete_fixture_is_only_record_consistency_not_physical_proof(self):
        result = validate(self.doc,self.root,SHA)
        self.assertEqual(result['record_integrity'],'consistent')
        self.assertIn('NOT_PROVEN',result['physical_hardware_authenticity'])
        self.assertTrue(result['m0_acceptance'].startswith('REQUIRES'))
    def test_missing_extra_or_unrun_case_rejected(self):
        for change in ['missing','extra','notrun','failed','boolean']:
            doc = copy.deepcopy(self.doc); case = next(iter(CASES))
            if change == 'missing': del doc['cases'][case]
            elif change == 'extra': doc['cases']['extra'] = doc['cases'][case]
            else: doc['cases'][case]['status'] = {'notrun':'NOT_RUN','failed':'FAIL','boolean':True}[change]
            with self.assertRaises(ValueError): validate(doc,self.root,SHA)
    def test_wrong_commit_debug_and_boolean_schema_rejected(self):
        for key,value in [('commit','b'*40),('build','debug'),('schema',True)]:
            doc=copy.deepcopy(self.doc);doc[key]=value
            with self.assertRaises(ValueError):validate(doc,self.root,SHA)
    def test_emulator_or_missing_device_not_accepted(self):
        for key,value in [('kind','simulator'),('model',''),('platform','linux'),('coverage','unknown')]:
            doc=copy.deepcopy(self.doc);doc['device'][key]=value
            with self.assertRaises(ValueError):validate(doc,self.root,SHA)
    def test_artifact_and_log_hashes_are_checked(self):
        for path in ['app.bin','log.txt']:
            old=(self.root/path).read_bytes();(self.root/path).write_bytes(b'changed')
            with self.assertRaises(ValueError):validate(self.doc,self.root,SHA)
            (self.root/path).write_bytes(old)
    def test_traversal_absolute_and_ambiguous_paths_rejected(self):
        for path in ['../log.txt','/log.txt','C:/log.txt','a\\log.txt','./log.txt','a/../log.txt','a//log.txt']:
            with self.assertRaises(ValueError):evidence_file(self.root,{'path':path,'sha256':self.ref['sha256']},1000)
    def test_empty_or_oversized_logs_rejected(self):
        with self.assertRaises(ValueError):evidence_file(self.root,self.ref,1)
        (self.root/'log.txt').write_bytes(b''); self.ref['sha256']=digest(self.root/'log.txt')
        with self.assertRaises(ValueError):evidence_file(self.root,self.ref,100)
    @unittest.skipIf(sys.platform == 'win32','Unix symlink fixture; no Windows privilege assumption')
    def test_symlink_not_followed(self):
        (self.root/'link').symlink_to(self.root/'log.txt')
        with self.assertRaises(ValueError):evidence_file(self.root,{'path':'link','sha256':self.ref['sha256']},1000)
    def test_null_boolean_nan_and_short_samples_rejected(self):
        for key,value in [('idle_memory_bytes',None),('installed_bytes',True),('idle_cpu_percent',float('nan')),
                          ('idle_sample_seconds',599),('cold_start_ms',[10]*9),('cold_start_ms',[0]*10),
                          ('memory_metric','RSS')]:
            doc=copy.deepcopy(self.doc);doc['metrics'][key]=value
            with self.assertRaises(ValueError):validate(doc,self.root,SHA)
    def test_duplicate_json_keys_rejected(self):
        with self.assertRaises(ValueError):json.loads('{"a":1,"a":2}',object_pairs_hook=unique_object)
    def test_timestamp_needs_timezone(self):
        doc=copy.deepcopy(self.doc);doc['cases'][next(iter(CASES))]['observed_at']='2026-11-02T12:00:00'
        with self.assertRaises(ValueError):validate(doc,self.root,SHA)
    def test_over_budget_not_hidden_by_record_consistency(self):
        self.doc['metrics']['idle_memory_bytes']=200*1024*1024
        self.doc['metrics']['idle_cpu_percent']=2
        result=validate(self.doc,self.root,SHA)
        self.assertFalse(any(result['budget_observations'].values()))
if __name__=='__main__':unittest.main()
