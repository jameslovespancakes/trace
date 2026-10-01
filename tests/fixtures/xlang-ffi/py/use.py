"""Fixture (P5, ffi): ctypes / cffi symbol lookups (always `possible`)."""
import ctypes

from cffi import FFI

lib = ctypes.CDLL("./libnative.so")


def run():
    lib.compress_buf(10)  # one C definition -> possible (weak kind)
    lib.twice(2)  # two C definitions -> two possible rows
    lib.not_there()  # negative control: no C definition -> no bridge


def run_cffi():
    ffi = FFI()
    ffi.cdef("int compress_buf(int n);")
    return ffi
