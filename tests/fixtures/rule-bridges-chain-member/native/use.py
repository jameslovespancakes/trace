"""Rule fixture (member-lookup hook): the looked-up symbol is the member the call spells."""
import ctypes

lib = ctypes.CDLL("./libnative.so")


def compress():
    return lib.compress_buf(10)
