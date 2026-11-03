#!/usr/bin/env python3
"""Validate ONE physical-device evidence bundle, not hardware authenticity or M0 approval."""
from __future__ import annotations
import argparse
import hashlib
import json
import math
from pathlib import Path, PurePosixPath
import re
import stat

CASES = frozenset({'install-cold-start', 'lan-permission-allow-deny-revoke',
    'directory-create-verify-clean', 'directory-restart-revoke',
    'provider-offline-timeout-cleanup', 'background-lock-kill',
    'two-device-wifi-without-internet', 'idle-resource-budget'})
HEX40 = re.compile(r'[0-9a-f]{40}')
HEX64 = re.compile(r'[0-9a-f]{64}')
MAX_LOG_BYTES = 16 * 1024 * 1024
MAX_ARTIFACT_BYTES = 256 * 1024 * 1024


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''): value.update(chunk)
    return value.hexdigest()


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result: raise ValueError('duplicate-json-key')
        result[key] = value
    return result


def evidence_file(root: Path, ref: dict, limit: int) -> Path:
    if not isinstance(ref, dict) or set(ref) != {'path','sha256'}:
        raise ValueError('invalid-evidence-reference')
    name, expected = ref['path'], ref['sha256']
    if not isinstance(name, str) or not isinstance(expected, str) or not HEX64.fullmatch(expected):
        raise ValueError('invalid-evidence-reference')
    relative = PurePosixPath(name)
    if not name or '\\' in name or ':' in name or relative.is_absolute() or any(p in ('..','.') for p in name.split('/')):
        raise ValueError('unsafe-evidence-path')
    if relative.as_posix() != name: raise ValueError('non-canonical-evidence-path')
    path = root
    for component in relative.parts:
        path = path / component
        if path.is_symlink(): raise ValueError('symlink-evidence-disallowed')
    if not path.resolve().is_relative_to(root.resolve()) or not path.is_file():
        raise ValueError('evidence-file-missing-or-outside-root')
    info = path.stat()
    if not stat.S_ISREG(info.st_mode) or not 0 < info.st_size <= limit:
        raise ValueError('evidence-file-size-or-type')
    if digest(path) != expected: raise ValueError('evidence-digest-mismatch')
    return path


def finite_number(value, *, positive=False) -> bool:
    return type(value) in (int, float) and math.isfinite(value) and (value > 0 if positive else value >= 0)


def validate(document: dict, root: Path, expected_commit: str) -> dict:
    if not HEX40.fullmatch(expected_commit): raise ValueError('expected-commit-invalid')
    if not isinstance(document, dict) or set(document) != {'schema','commit','build','device','artifact','cases','metrics'}:
        raise ValueError('record-schema')
    if type(document['schema']) is not int or document['schema'] != 1:
        raise ValueError('schema-version')
    if document['commit'] != expected_commit or document['build'] != 'release':
        raise ValueError('wrong-commit-or-build')
    device = document['device']
    if not isinstance(device, dict) or set(device) != {'kind','platform','model','os_build','coverage'}:
        raise ValueError('device-schema')
    if device['kind'] != 'physical' or device['platform'] not in ('android','ios') or device['coverage'] not in ('minimum','current'):
        raise ValueError('not-a-physical-mobile-target-record')
    for key in ('model','os_build'):
        if not isinstance(device[key], str) or not device[key].strip() or len(device[key]) > 128:
            raise ValueError('device-identity-missing')
    evidence_file(root, document['artifact'], MAX_ARTIFACT_BYTES)
    cases = document['cases']
    if not isinstance(cases, dict) or set(cases) != CASES:
        raise ValueError('required-case-set-mismatch')
    files = []
    for name, row in cases.items():
        if not isinstance(row, dict) or set(row) != {'status','observed_at','evidence'}:
            raise ValueError('case-schema')
        if row['status'] != 'PASS': raise ValueError('required-case-not-passed')
        # Time is attributed to the operator; syntax is validated, not authenticity.
        from datetime import datetime
        if not isinstance(row['observed_at'], str): raise ValueError('case-time-missing')
        timestamp = datetime.fromisoformat(row['observed_at'].replace('Z','+00:00'))
        if timestamp.utcoffset() is None: raise ValueError('case-time-zone-required')
        path = evidence_file(root, row['evidence'], MAX_LOG_BYTES)
        files.append(path)
    metrics = document['metrics']
    if not isinstance(metrics, dict) or set(metrics) != {'memory_metric','idle_memory_bytes','idle_cpu_percent',
            'idle_sample_seconds','cold_start_ms','installed_bytes'}:
        raise ValueError('metrics-schema')
    expected_metric = 'PSS' if device['platform'] == 'android' else 'physical-footprint'
    if metrics['memory_metric'] != expected_metric: raise ValueError('wrong-memory-metric')
    for key in ('idle_memory_bytes','installed_bytes'):
        if type(metrics[key]) is not int or metrics[key] <= 0: raise ValueError('missing-positive-byte-measurement')
    if not finite_number(metrics['idle_cpu_percent']): raise ValueError('invalid-cpu-measurement')
    if not finite_number(metrics['idle_sample_seconds']) or metrics['idle_sample_seconds'] < 600:
        raise ValueError('idle-sampling-too-short')
    times = metrics['cold_start_ms']
    if not isinstance(times, list) or len(times) != 10 or not all(finite_number(x, positive=True) for x in times):
        raise ValueError('ten-cold-start-measurements-required')
    return {'schema':1, 'commit':expected_commit, 'record_integrity':'consistent',
            'required_cases_present':len(cases), 'platform':device['platform'], 'coverage':device['coverage'],
            'budget_observations':{'idle_memory_within_150_mib':metrics['idle_memory_bytes'] <= 150*1024*1024,
                                   'idle_cpu_within_1_percent':metrics['idle_cpu_percent'] <= 1},
            'physical_hardware_authenticity':'NOT_PROVEN_BY_THIS_CHECKER',
            'm0_acceptance':'REQUIRES_REVIEW_OF_ALL_TARGETS_AND_RAW_EVIDENCE'}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('record', type=Path)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--commit', required=True)
    args = parser.parse_args()
    try:
        if args.record.stat().st_size > 1024*1024: raise ValueError('record-too-large')
        document = json.loads(args.record.read_text(encoding='utf-8'), object_pairs_hook=unique_object)
        result = validate(document, args.root, args.commit)
        print(json.dumps(result, indent=2)); return 0
    except (OSError, ValueError, TypeError, KeyError, OverflowError) as error:
        # Do not echo user paths, identifiers or OS/provider exception text.
        print(json.dumps({'record_integrity':'rejected', 'error_type':type(error).__name__,
                          'm0_acceptance':'NOT_GRANTED'})); return 1


if __name__ == '__main__':
    raise SystemExit(main())
