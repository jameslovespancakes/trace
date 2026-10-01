"""Fixture (bridges gate, ctypes / cffi families): runtime symbol lookups."""
import ctypes

from cffi import FFI

lib = ctypes.CDLL("./libnative.so")


def with_ctypes():
    return lib.compress_buf(10)


def with_cffi():
    ffi = FFI()
    ffi.cdef("int compress_buf(int n);")
    clib = ffi.dlopen("./libnative.so")
    return clib.compress_buf(1)
