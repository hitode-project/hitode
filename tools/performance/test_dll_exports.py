"""Tests for export comparison, protocol validation and the real x86 C driver.

Usage: python -m unittest discover -s tools/performance -p test_dll_exports.py -v
Input: repository sources plus LLVM/Windows SDK for native tests; optional
ADV_DLL_DRIVER_RUNNER points to a built adv-run for interpreter/JIT verification.
Output: unittest counts/status only; isolated artifacts stay in target/scratch.
"""
from __future__ import annotations

import copy
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import dll_exports as driver


def result(kind: str = 'u32', **changes):
    row = {'event': 'result', 'id': 'call', 'iterations': 100, 'warmup': 3,
           'return_kind': kind, 'eax': 123, 'edx': 456, 'is_null': 0,
           'qpc_ticks_hex': '00000000000f4240', 'qpc_frequency_hex': '0000000000989680',
           'observed_loop_ns': 200_000_000,
           'hashes': [{'buffer': 'out', 'offset': 4, 'size': 8, 'fnv1a64': '0123456789abcdef'}]}
    return {**row, **changes}


def runs(actual=None):
    return {'native': {'calls': [result()], 'max_poll_gap_ns': 1_000_000},
            'jit': {'calls': [actual or result()], 'max_poll_gap_ns': 1_000_000}}


class ComparisonTests(unittest.TestCase):
    def test_cost_uses_native_qpc_and_guest_host_observation(self):
        actual = result(qpc_ticks_hex='000000000000000a')
        summary = driver.compare(runs(actual), 20)
        self.assertTrue(summary['all_match'])
        self.assertEqual(summary['module_cost_ratio']['jit'], 2)
        sample = runs(actual)
        sample['jit']['max_poll_gap_ns'] = 15_000_000
        # The native QPC window does not inherit the guest polling resolution.
        self.assertEqual(driver.compare(sample, 20)['module_cost_ratio']['jit'], 2)

    def test_differences_in_return_or_hash_block_ratios(self):
        for actual in (result(eax=124), result(hashes=[]), result(iterations=99)):
            with self.subTest(actual=actual):
                summary = driver.compare(runs(actual), 20)
                self.assertFalse(summary['all_match'])
                self.assertIsNone(summary['module_cost_ratio']['jit'])

    def test_pointer_and_void_returns_do_not_compare_register_addresses(self):
        for kind in ('pointer', 'void'):
            self.assertEqual(driver.signature(result(kind)), driver.signature(result(kind, eax=1, edx=2)))
        self.assertNotEqual(driver.signature(result('pointer')), driver.signature(result('pointer', is_null=1)))

    def test_u32_ignores_undefined_edx_but_u64_compares_it(self):
        self.assertEqual(driver.signature(result()), driver.signature(result(edx=999)))
        self.assertNotEqual(driver.signature(result('u64')), driver.signature(result('u64', edx=999)))

    def test_short_or_delayed_observations_are_not_cost_measurements(self):
        for sample in (runs(result(observed_loop_ns=0)), runs(result(observed_loop_ns=1_000_000))):
            self.assertIsNone(driver.compare(sample, 20)['module_cost_ratio']['jit'])
        sample = runs()
        sample['jit']['max_poll_gap_ns'] = 50_000_000
        self.assertIsNone(driver.compare(sample, 20)['module_cost_ratio']['jit'])

    def test_any_function_difference_blocks_module_ratio(self):
        sample = runs()
        sample['native']['calls'].append(result(id='second'))
        sample['jit']['calls'].append(result(id='second', eax=99))
        self.assertIsNone(driver.compare(sample, 20)['module_cost_ratio']['jit'])

    def test_strict_report_rejects_time_limit_warning_and_failure(self):
        report = {'stop_reason': 'guest_exit', 'guest_exit_code': 0, 'api': {'mode': 'strict', 'acceptance_eligible': True, 'warnings': []}, 'host_calls': 37}
        self.assertEqual(driver.check_report(report), {'host_calls': 37})
        for changed in ({'stop_reason': 'time_limit'}, {'guest_exit_code': 2}, {'failure': 'failed'},
                        {'compile_failures': 1}, {'api': {'acceptance_eligible': True, 'warnings': ['warning']}}):
            with self.assertRaises(ValueError):
                driver.check_report({**report, **changed})

    def test_sample_inputs_require_adv_samples_and_stay_inside_it(self):
        with patch.dict(os.environ, {}, clear=True), self.assertRaises(ValueError):
            driver.sample_directory('module')
        for name in ('../module', '/module', 'C:\\module', '..\\module'):
            with self.subTest(name=name), self.assertRaises(ValueError):
                driver.relative_input(name)

    def test_runner_arguments_cannot_replace_fixed_execution_policy(self):
        driver.validate_runner_args(['--sync-compilation', '--codepage', '936'])
        for argument in ('--backend', '--api-mode=discovery', '--codec=native', '--trace'):
            with self.subTest(argument=argument), self.assertRaises(ValueError):
                driver.validate_runner_args([argument])


class ProtocolTests(unittest.TestCase):
    def test_fragmented_markers_and_hash_results(self):
        events = driver.Events()
        events.consume(b'{"event":"beg', 1)
        events.consume(b'in","id":"call"}\n', 10)
        events.consume(b'{"event":"end","id":"call"}\n', 110)
        events.consume(json.dumps(result()).encode() + b'\n{"event":"complete","calls":1}\n', 9999)
        self.assertEqual(events.finish(['call'])[0]['observed_loop_ns'], 100)

    def test_missing_out_of_order_and_error_events_are_rejected(self):
        for event in ({'event': 'end', 'id': 'call'}, result(), {'event': 'complete', 'calls': 0},
                      {'event': 'error', 'message': 'missing field'}):
            with self.subTest(event=event), self.assertRaises(ValueError):
                driver.Events().consume(json.dumps(event).encode() + b'\n', 10)
        with self.assertRaises(ValueError):
            driver.Events().finish(['call'])


@unittest.skipUnless(os.name == 'nt', 'native x86 PE verification requires Windows')
class NativeDriverTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        scratch = driver.ROOT / 'target/scratch/dll-driver-tests'
        scratch.mkdir(parents=True, exist_ok=True)
        cls.temp = tempfile.TemporaryDirectory(dir=scratch)
        cls.directory = Path(cls.temp.name)
        cls.build = cls.directory / 'build'
        subprocess.run(['powershell.exe', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File',
                        str(driver.ROOT / 'tools/fixtures/build-dll-driver.ps1'), '-OutputDirectory', str(cls.build)],
                       check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        cls.script = json.loads((driver.ROOT / 'tools/fixtures/dll-driver-example.json').read_text())

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def execute(self, script):
        image = self.directory / self.id().rsplit('.', 1)[-1]
        image.mkdir(exist_ok=True)
        for name in ('fixture_dll_driver.exe', 'fixture_export_probe.dll'):
            shutil.copy2(self.build / name, image / name)
        (image / 'dll-driver.json').write_text(script if isinstance(script, str) else json.dumps(script), encoding='utf-8')
        status = subprocess.run([str(image / 'fixture_dll_driver.exe')], cwd=image, timeout=10,
                                creationflags=subprocess.CREATE_NO_WINDOW, capture_output=True).returncode
        events = [json.loads(line) for line in (image / driver.EVENT_FILE).read_text().splitlines()]
        return status, events

    def test_all_abis_and_result_references_native(self):
        status, events = self.execute(self.script)
        self.assertEqual(status, 0)
        rows = {e['id']: e for e in events if e['event'] == 'result'}
        self.assertEqual(rows['advance']['eax'], 46)
        self.assertEqual(rows['indirect']['eax'], 92)
        self.assertEqual(rows['wide']['edx'], 0x9abcdef0)
        self.assertNotIn('eax', rows['pointer'])
        self.assertEqual(events[-1], {'event': 'complete', 'calls': 7})

    def test_incorrect_abi_is_reported_after_restoring_the_stack(self):
        script = copy.deepcopy(self.script)
        script['calls'] = [script['calls'][0]]
        script['calls'][0]['abi'] = 'cdecl'
        status, events = self.execute(script)
        self.assertEqual(status, 2)
        self.assertEqual(events[-1]['message'], 'ABI stack cleanup mismatch')

    def test_invalid_scripts_fail_with_an_error_record(self):
        mutations = [
            lambda s: s.update(version=2),
            lambda s: s.update(calls=[]),
            lambda s: s['calls'][0].update(iterations=0),
            lambda s: s['calls'][0].update(abi='fastcall'),
            lambda s: s['calls'][0].update(export='Missing'),
            lambda s: s['calls'][0].update(args=[0] * 65),
            lambda s: s['calls'][0].update(args=[4294967296]),
            lambda s: s['calls'][0].update(args=[{'buffer': 'missing'}]),
            lambda s: s['calls'][0].update(args=[{'read': 'state', 'offset': 14}]),
            lambda s: s['calls'][0].update(hashes=[{'buffer': 'state', 'size': 17}]),
            lambda s: s['calls'][0].update(args=[{'result': 'advance'}]),
            lambda s: s['buffers'][0].update(fill=256),
            lambda s: s['buffers'][0].update(hex='1'),
            lambda s: s['buffers'][0].update(size=0),
            lambda s: s['buffers'][1].update(id='state'),
            lambda s: s['calls'][1].update(id='add'),
            lambda s: s.update(iteration=1),
        ]
        for mutate in mutations:
            script = copy.deepcopy(self.script)
            mutate(script)
            with self.subTest(mutation=mutations.index(mutate)):
                status, events = self.execute(script)
                self.assertEqual(status, 2)
                self.assertEqual(events[-1]['event'], 'error')
        for text in ('{"version":1,"version":1}', '{"version":1} garbage', '[', '{"dll":"\\ud800"}',
                     '{"version":01}', '{"version":true}', '{"dll":"\\u0000"}'):
            with self.subTest(json=text):
                status, events = self.execute(text)
                self.assertEqual(status, 2)
                self.assertEqual(events[-1]['event'], 'error')

    def test_file_and_hex_initializers_and_pointer_offsets(self):
        image = self.directory / self.id().rsplit('.', 1)[-1]
        image.mkdir()
        (image / '输入.bin').write_bytes(bytes([1, 2, 3, 4]))
        script = {'version': 1, 'dll': 'fixture_export_probe.dll',
                  'buffers': [{'id': 'file', 'size': 8, 'file': '输入.bin'},
                              {'id': 'hex', 'size': 8, 'hex': '01020304'}],
                  'calls': [{'id': 'offset', 'export': 'Advance', 'abi': 'thiscall',
                             'this': {'buffer': 'hex', 'offset': 4}, 'args': [-1], 'expect': '0xffffffff'},
                            {'id': 'add', 'export': 'Add', 'abi': 'stdcall',
                             'args': [{'read': 'file'}, {'read': 'hex'}], 'expect': '0x08060402',
                             'hashes': [{'buffer': 'file'}, {'buffer': 'hex', 'size': 4}]}]}
        status, events = self.execute(script)
        self.assertEqual(status, 0)
        self.assertEqual(events[-2]['hashes'][0]['fnv1a64'], '6a9750b4c81e78ad')

    @unittest.skipUnless(os.environ.get('ADV_DLL_DRIVER_RUNNER'), 'set ADV_DLL_DRIVER_RUNNER to test both guest backends')
    def test_native_interpreter_jit_full_protocol_and_diff(self):
        runner = Path(os.environ['ADV_DLL_DRIVER_RUNNER']).resolve()
        script = copy.deepcopy(self.script)
        (self.build / '输入.bin').write_bytes(bytes([1, 2, 3, 4]))
        script['buffers'].extend([{'id': '日本語', 'size': 8, 'file': '输入.bin'},
                                  {'id': 'hex', 'size': 4, 'hex': '01020304'}])
        script['calls'].append({'id': 'file-hex', 'export': 'Add', 'abi': 'stdcall',
                                'args': [{'read': '日本語'}, {'read': 'hex'}], 'expect': '0x08060402',
                                'hashes': [{'buffer': '日本語'}, {'buffer': 'hex'}]})
        collected = {
            variant: driver.run_variant(variant, self.directory / variant, self.build / 'fixture_dll_driver.exe',
                                        self.build, script, runner, 30, ['--sync-compilation'])
            for variant in ('native', 'interpreter', 'jit')
        }
        self.assertTrue(driver.compare(collected, 20)['all_match'])
        for variant in ('interpreter', 'jit'):
            self.assertGreater(collected[variant]['runtime']['host_calls'], 0)
        self.assertGreater(collected['jit']['runtime']['native_entries'], 0)
        self.assertEqual(collected['interpreter']['runtime']['native_entries'], 0)


if __name__ == '__main__':
    unittest.main()
