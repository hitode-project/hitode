"""List wait/timing-related imports of each PE under $ADV_SAMPLES.

Imports only hint at how an engine waits; the loop structure needs runtime tracing.
"""
import os, pathlib, re, sys

import pefile

WAIT = ['Sleep', 'SleepEx', 'SwitchToThread', 'timeGetTime', 'GetTickCount', 'QueryPerformanceCounter',
        'timeBeginPeriod', 'timeSetEvent', 'SetTimer', 'CreateWaitableTimerA', 'CreateWaitableTimerW',
        'PeekMessageA', 'PeekMessageW', 'GetMessageA', 'GetMessageW', 'WaitMessage',
        'MsgWaitForMultipleObjects', 'MsgWaitForMultipleObjectsEx',
        'WaitForSingleObject', 'WaitForMultipleObjects', 'DwmFlush']
SKIP = re.compile(r'(?i)(setup|uninst|unins\d+|config|update|checker|support|launcher)')


def main():
    root = os.environ.get('ADV_SAMPLES') or sys.exit('set ADV_SAMPLES')
    for game in sorted(p for p in pathlib.Path(root).iterdir() if p.is_dir()):
        for p in sorted(game.rglob('*')):
            if p.suffix.lower() not in ('.exe', '.dll') or SKIP.search(p.name) or not p.is_file():
                continue
            try:
                pe = pefile.PE(str(p), fast_load=True)
                pe.parse_data_directories(directories=[1, 13])
            except pefile.PEFormatError:
                continue
            names = {i.name.decode() for d in getattr(pe, 'DIRECTORY_ENTRY_IMPORT', [])
                     + getattr(pe, 'DIRECTORY_ENTRY_DELAY_IMPORT', []) for i in d.imports if i.name}
            hit = [w for w in WAIT if w in names]
            if any(w in names for w in ('PeekMessageA', 'PeekMessageW', 'GetMessageA', 'GetMessageW', 'Sleep')):
                print(f'{game.name[:20]:20} {str(p.relative_to(game))[:34]:34} {" ".join(hit)}')


if __name__ == '__main__':
    main()
