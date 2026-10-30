#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
# Usage: headless-seat.py <ready-file>
# Keep a virtual keyboard and pointer alive on this private Wayland display. A
# headless wlroots backend supplies an output, not input devices: without these
# wl_seat has no keyboard/pointer capability, so winit cannot focus or lock it.
# Protocol references (wire requests, not copied implementation):
# https://gitlab.freedesktop.org/wayland/wayland/-/blob/main/protocol/wayland.xml
# https://github.com/swaywm/wlr-protocols/blob/master/unstable/wlr-virtual-pointer-unstable-v1.xml
# https://github.com/swaywm/wlroots/blob/master/protocol/virtual-keyboard-unstable-v1.xml
import array
import ctypes
import os
from pathlib import Path
import socket
import struct
import sys


def words(*values):
    return struct.pack("=" + "I" * len(values), *values)


def string(value):
    data = value.encode() + b"\0"
    return words(len(data)) + data + b"\0" * (-len(data) % 4)


def keymap():
    """Compile the default keyboard layout with the dev shell's libxkbcommon."""
    xkb = ctypes.CDLL("libxkbcommon.so.0")
    xkb.xkb_context_new.argtypes = [ctypes.c_int]
    xkb.xkb_context_new.restype = ctypes.c_void_p
    xkb.xkb_keymap_new_from_names.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_int]
    xkb.xkb_keymap_new_from_names.restype = ctypes.c_void_p
    xkb.xkb_keymap_get_as_string.argtypes = [ctypes.c_void_p, ctypes.c_int]
    xkb.xkb_keymap_get_as_string.restype = ctypes.c_void_p
    xkb.xkb_keymap_unref.argtypes = [ctypes.c_void_p]
    xkb.xkb_context_unref.argtypes = [ctypes.c_void_p]
    libc = ctypes.CDLL(None)
    libc.free.argtypes = [ctypes.c_void_p]
    ctx = xkb.xkb_context_new(0)
    if not ctx:
        raise RuntimeError("xkb_context_new failed")
    km = xkb.xkb_keymap_new_from_names(ctx, None, 0)
    if not km:
        xkb.xkb_context_unref(ctx)
        raise RuntimeError("xkb_keymap_new_from_names failed")
    ptr = xkb.xkb_keymap_get_as_string(km, 1)  # XKB_KEYMAP_FORMAT_TEXT_V1
    if not ptr:
        xkb.xkb_keymap_unref(km)
        xkb.xkb_context_unref(ctx)
        raise RuntimeError("xkb_keymap_get_as_string failed")
    data = ctypes.string_at(ptr) + b"\0"
    libc.free(ptr)
    xkb.xkb_keymap_unref(km)
    xkb.xkb_context_unref(ctx)
    return data


def main():
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.settimeout(10)
    sock.connect(os.path.join(os.environ["XDG_RUNTIME_DIR"], os.environ["WAYLAND_DISPLAY"]))
    next_id = 2  # wl_display is object 1
    globals_ = {}
    capabilities = 0
    seat = None

    def allocate():
        nonlocal next_id
        value = next_id
        next_id += 1
        return value

    def send(obj, opcode, data=b"", fd=None):
        message = words(obj, ((len(data) + 8) << 16) | opcode) + data
        if fd is None:
            sock.sendall(message)
        else:
            sent = sock.sendmsg([message], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", [fd]))])
            sock.sendall(message[sent:])

    buffered = b""

    def event():
        nonlocal buffered, capabilities
        while len(buffered) < 8 or len(buffered) < (struct.unpack_from("=I", buffered, 4)[0] >> 16):
            chunk = sock.recv(65536)
            if not chunk:
                raise RuntimeError("the compositor closed the seat connection")
            buffered += chunk
        obj, header = struct.unpack_from("=II", buffered)
        size, opcode = header >> 16, header & 65535
        data = buffered[8:size]
        buffered = buffered[size:]
        if obj == 1 and opcode == 0:  # wl_display.error
            raise RuntimeError(f"Wayland protocol error: {data!r}")
        if obj == registry and opcode == 0:  # wl_registry.global
            name, length = struct.unpack_from("=II", data)
            iface = data[8:8 + length - 1].decode()
            globals_[iface] = name
        if obj == seat and opcode == 0:  # wl_seat.capabilities
            capabilities = struct.unpack_from("=I", data)[0]
        return obj, opcode

    def roundtrip():
        callback = allocate()
        send(1, 0, words(callback))  # wl_display.sync
        while event() != (callback, 0):
            pass

    def bind(iface):
        if iface not in globals_:
            raise RuntimeError(f"the compositor does not advertise {iface}")
        obj = allocate()
        send(registry, 0, words(globals_[iface]) + string(iface) + words(1, obj))
        return obj

    registry = allocate()
    send(1, 1, words(registry))  # wl_display.get_registry
    roundtrip()
    seat = bind("wl_seat")
    keyboard_manager = bind("zwp_virtual_keyboard_manager_v1")
    pointer_manager = bind("zwlr_virtual_pointer_manager_v1")
    keyboard, pointer = allocate(), allocate()
    send(keyboard_manager, 0, words(seat, keyboard))
    send(pointer_manager, 0, words(seat, pointer))
    data = keymap()
    fd = os.memfd_create("cod4e-headless-keymap")
    try:
        os.write(fd, data)
        send(keyboard, 0, words(1, len(data)), fd)  # keymap, XKB_V1
    finally:
        os.close(fd)
    roundtrip()
    if capabilities & 3 != 3:
        raise RuntimeError(f"headless seat lacks keyboard/pointer capabilities: {capabilities}")
    Path(sys.argv[1]).touch()
    print("headless seat: keyboard and pointer available", flush=True)
    sock.settimeout(None)
    # The devices live as long as their connection. No synthetic key presses or
    # motion: the game's own control channel drives input through its normal queue.
    while True:
        event()


if __name__ == "__main__":
    main()
