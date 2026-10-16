#!/usr/bin/env python3
"""Launch the iOS player under a Mac-side debugger that speaks StikDebug's
universal JIT protocol (IOS-JIT-1), so test devices take the iOS 26 TXM path.

The app raises `brk #0xf00d` with the command in x16: 1 prepares a
read-execute region of x1 bytes (x0 = null asks the debugger to allocate)
and returns it in x0; 0 detaches. lldb only connects and attaches. From
then on the process runs on the same raw gdb-remote packets as StikDebug's
script, so a failure here is one StikDebug would meet too, and lldb never
handles a stop: doing so loads every library's image list and symbols while
the app waits, which took seconds per stop. On iOS 26 and later the
debugger also writes one byte per 16 KiB page, which is what TXM accepts as
debugger-prepared; earlier systems need only the attach.

usage: xcrun python3 tools/ios/jit_launch.py <device-udid> [--bundle ID]
       [--touch] [--log FILE]
"""
import argparse
import json
import os
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, subprocess.check_output(["xcrun", "lldb", "-P"], text=True).strip())
import lldb  # noqa: E402

BRK_F00D = 0xD43E01A0
PAGE = 16 << 10
PC = 0x20
X16 = 0x10


def devicectl(*arguments):
    with tempfile.TemporaryDirectory() as directory:
        path = os.path.join(directory, "result.json")
        subprocess.run(["xcrun", "devicectl", *arguments, "--json-output", path],
                       check=True, stdout=subprocess.DEVNULL)
        with open(path) as file:
            return json.load(file)["result"]


def identifier(udid):
    """lldb's `device select` takes the CoreDevice identifier, not the UDID."""
    for device in devicectl("list", "devices")["devices"]:
        if udid in (device["identifier"], device["hardwareProperties"].get("udid")):
            return device["identifier"]
    raise SystemExit(f"no device {udid}")


class Debugger:
    def __init__(self):
        self.debugger = lldb.SBDebugger.Create()
        self.debugger.SetAsync(False)
        # A continue packet waits until the app's next stop.
        self.command("settings set plugin.process.gdb-remote.packet-timeout 60")

    def command(self, text):
        result = lldb.SBCommandReturnObject()
        self.debugger.GetCommandInterpreter().HandleCommand(text, result)
        # Device commands report eReturnStatusStarted when they succeed.
        if result.GetStatus() == lldb.eReturnStatusFailed:
            raise SystemExit(f"{text}: {result.GetError().strip()}")
        return result.GetOutput()

    def packet(self, text):
        output = self.command(f'process plugin packet send "{text}"')
        return output.split("response:", 1)[1].strip()

    def register(self, tid, number):
        reply = self.packet(f"p{number:x};thread:{tid};")
        return int.from_bytes(bytes.fromhex(reply), "little")

    def write(self, tid, number, value):
        reply = self.packet(f"P{number:x}={value.to_bytes(8, 'little').hex()};thread:{tid};")
        if reply != "OK":
            raise SystemExit(f"write register {number:#x}: {reply}")

    def attach(self, udid, pid, timeout=30):
        self.command(f"device select {identifier(udid)}")
        # The attach completes asynchronously; serve only a stopped process.
        self.command(f"device process attach --pid {pid}")
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            process = self.debugger.GetSelectedTarget().GetProcess()
            if process.IsValid() and process.GetState() == lldb.eStateStopped:
                return process
            time.sleep(0.1)
        raise SystemExit("attach did not finish")


def host_info(debugger):
    reply = debugger.packet("qHostInfo")
    return dict(field.split(":", 1) for field in reply.rstrip(";").split(";"))


def serve(debugger, touch, mask):
    """Answers JIT calls until the app detaches or exits."""
    while True:
        reply = debugger.packet("c")
        if reply[:1] in ("W", "X"):
            print(f"process ended: {reply}")
            return
        fields = dict(field.split(":", 1) for field in reply[3:].split(";") if ":" in field)
        tid = fields.get("thread")
        pc = debugger.register(tid, PC) if tid else None
        word = pc is not None and debugger.packet(f"m{pc & mask:x},4")
        if not word or int.from_bytes(bytes.fromhex(word), "little") != BRK_F00D:
            # Not a JIT call: the app handles its own exception once detached.
            print(f"not a JIT call, detaching: {reply[:3]} at {pc}")
            debugger.packet("D")
            return
        command = debugger.register(tid, X16)
        debugger.write(tid, PC, pc + 4)
        if command == 0:
            print("app detached the JIT debugger")
            debugger.packet("D")
            return
        if command != 1:
            raise SystemExit(f"unknown JIT command {command}")
        address, size = debugger.register(tid, 0), debugger.register(tid, 1)
        started = time.monotonic()
        if address == 0:
            reply = debugger.packet(f"_M{size:x},rx")
            if reply.startswith("E"):
                raise SystemExit(f"allocate {size} bytes: {reply}")
            address = int(reply, 16)
        if touch:
            for offset in range(0, size, PAGE):
                reply = debugger.packet(f"M{address + offset:x},1:00")
                if reply != "OK":
                    raise SystemExit(f"prepare page {address + offset:#x}: {reply}")
        debugger.write(tid, 0, address)
        print(f"prepared {size >> 20} MiB at {address:#x} "
              f"in {time.monotonic() - started:.1f} s")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("udid")
    parser.add_argument("--bundle", default="org.hitode.launcher")
    parser.add_argument("--touch", action="store_true",
                        help="write every page even before iOS 26")
    parser.add_argument("--log", help="record gdb-remote packets to this file")
    arguments = parser.parse_args()
    started = time.monotonic()
    pid = devicectl("device", "process", "launch", "--device", arguments.udid,
                    "--start-stopped", "--terminate-existing",
                    arguments.bundle)["process"]["processIdentifier"]
    print(f"launched {arguments.bundle} stopped as pid {pid} "
          f"in {time.monotonic() - started:.1f} s")
    started = time.monotonic()
    debugger = Debugger()
    if arguments.log:
        debugger.command(f"log enable -T -f {arguments.log} gdb-remote packets")
    debugger.attach(arguments.udid, pid)
    print(f"attached in {time.monotonic() - started:.1f} s")
    started = time.monotonic()
    info = host_info(debugger)
    touch = arguments.touch or int(info["os_version"].split(".")[0]) >= 26
    mask = (1 << int(info.get("addressing_bits", 47))) - 1
    serve(debugger, touch, mask)
    print(f"served until detach in {time.monotonic() - started:.1f} s")
    # lldb still believes it holds a stopped process; skip its teardown.
    os._exit(0)


if __name__ == "__main__":
    main()
