#!/usr/bin/env python3
"""Functional Android shell acceptance using our own PE fixture, never a game benchmark."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import time

PACKAGE = 'org.hitode.player'
DATA = 'files/data/shell-probe/'

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--adb', required=True)
    p.add_argument('--serial', required=True)
    p.add_argument('--adb-port', default='5037')
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--rotate', action='store_true')
    a = p.parse_args()
    repo = Path(__file__).resolve().parents[2]
    output = a.output.resolve()
    if not output.is_relative_to(repo / 'target'):
        p.error('evidence must be under ignored target/')
    output.mkdir(parents=True, exist_ok=False)
    prefix = [a.adb, '-P', a.adb_port, '-s', a.serial]
    def adb(*words):
        return subprocess.check_output(prefix + list(words), stderr=subprocess.PIPE, timeout=20)
    def private(path):
        return adb('exec-out', 'run-as', PACKAGE, 'cat', path)
    def state():
        return json.loads(private('files/shell-state.json'))
    def words():
        return struct.unpack('<12I', private(DATA + 'overlay/probe-state.bin'))
    def wait(read, predicate, label):
        end = time.monotonic() + 15
        last = None
        while time.monotonic() < end:
            try:
                last = read()
                if predicate(last):
                    return last
            except (subprocess.CalledProcessError, json.JSONDecodeError, struct.error):
                pass
            time.sleep(.2)
        raise RuntimeError(f'{label} did not complete: {last!r}')
    def action(name):
        adb('shell', 'am', 'start', '-W', '-n', PACKAGE + '/.GameActivity', '--es', 'probe_action', name)
    def capture(name, value):
        (output / (name + '.json')).write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    results = {'fixture': 'tools/fixtures/android_shell.c', 'performance_test': False, 'checks': []}
    try:
        initial = wait(state, lambda s: s.get('status') == 'running' and s.get('presented_serial', 0) > 0
                       and s.get('audio', {}).get('non_silent_samples', 0) > 0, 'presentation/audio')
        assert initial['api']['stub_calls'] == 0 and initial['api']['acceptance_eligible']
        assert initial['audio']['device_running'] and initial['audio']['device_errors'] == 0
        capture('initial', initial)
        baseline = words()
        assert baseline[0:2] == (0x41565350, 1)
        action('advance')
        advanced = wait(words, lambda s: s[2] == baseline[2] + 1, 'Enter')
        action('pointer')
        wait(words, lambda s: s[3] == advanced[3] + 1, 'letterboxed pointer')
        action('save')
        saved = wait(words, lambda s: s[7] == advanced[7] + 1, 'menu save')
        action('advance')
        wait(words, lambda s: s[2] == saved[2] + 1, 'second Enter')
        action('load')
        loaded = wait(words, lambda s: s[2] == saved[2] and s[9] == saved[2]
                      and s[8] > saved[8], 'file/registry load')
        results['checks'] += ['presented pixels', 'real audio callbacks', 'Enter', 'mapped pointer', 'guest menu', 'file and registry save/load']
        action('ctrl_down')
        wait(words, lambda s: s[6] == 1, 'held Ctrl')
        adb('shell', 'input', 'keyevent', 'KEYCODE_HOME')
        paused = wait(lambda: json.loads(private(DATA + 'android-session.json')),
                      lambda s: s.get('paused') and not s.get('audio', {}).get('running'), 'background pause')
        capture('paused', paused)
        before = words()
        time.sleep(.7)
        assert words() == before, 'guest continued executing in background'
        adb('shell', 'am', 'start', '-W', '-n', PACKAGE + '/.GameActivity')
        resumed = wait(state, lambda s: s.get('status') == 'running' and s.get('resumes', 0) > paused['resumes']
                       and s.get('audio', {}).get('starts', 0) > paused['audio']['starts'], 'resume/audio reopen')
        wait(words, lambda s: s[6] == 0, 'Ctrl release on pause')
        capture('resumed', resumed)
        results['checks'] += ['background guest pause', 'audio suspend/reopen', 'Ctrl release on focus loss']
        if a.rotate:
            action('rotate')
            rotated = wait(state, lambda s: s.get('presented_serial', 0) > resumed['presented_serial'], 'surface recreation')
            action('pointer')
            wait(words, lambda s: s[3] > loaded[3], 'rotated pointer')
            capture('rotated', rotated)
            action('rotate')
            results['checks'] += ['orientation surface recreation and mapped pointer']
        (output / 'fixture-screen.png').write_bytes(adb('exec-out', 'screencap', '-p'))
        action('stop')
        final = wait(lambda: json.loads(private(DATA + 'android-session.json')),
                     lambda s: s.get('status') == 'stopped', 'clean stop')
        assert final['guest_exit_code'] == 0 and final['failure'] is None
        assert final['api']['stub_calls'] == 0 and not final['audio']['device_running']
        capture('final', final)
        adb('shell', 'am', 'start', '-W', '-n', PACKAGE + '/.GameActivity', '--es', 'fixture', 'shell-probe')
        restarted = wait(state, lambda s: s.get('session') != final['session'] and s.get('status') == 'running', 'restart')
        wait(words, lambda s: s[2] == saved[2] and s[9] == saved[2] and s[8] >= 1, 'persisted save/registry')
        capture('restarted', restarted)
        action('stop')
        wait(lambda: json.loads(private(DATA + 'android-session.json')),
             lambda s: s.get('session') == restarted['session'] and s.get('status') == 'stopped', 'restart stop')
        results['checks'] += ['graceful guest exit', 'restart persisted save and registry']
        results['passed'] = True
    except BaseException as error:
        results['passed'] = False
        results['error'] = str(error)
        try:
            capture('blocked', state())
            action('stop')
        except Exception:
            pass
        raise
    finally:
        capture('result', results)
        print(json.dumps(results, ensure_ascii=False))

if __name__ == '__main__':
    main()
