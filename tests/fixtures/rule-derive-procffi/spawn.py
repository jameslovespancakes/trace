"""Rule fixture (procffi derivation rules): spawn wrappers, lookups through base classes."""
import _net
import _proc
from _ffi import FuncPtr as _FuncPtr


class Popen:
    def __init__(self, args, executable=None):
        _proc.spawn(args)


def run(*popenargs, **kwargs):
    """Spread of the own rest parameter: the first rest argument is the command."""
    with Popen(*popenargs, **kwargs) as p:
        return p


def run_list(cmd):
    """A spread list parameter: its elements, not the parameter, reach the command."""
    Popen(*cmd)


def run_after(first, *more):
    """A spread followed by another positional argument: later positions unknown."""
    Popen(*more, first)


class Library:
    def __init__(self, path):
        class _Ptr(_FuncPtr):
            flags = 1
        self._Ptr = _Ptr

    def __getattr__(self, name):
        return self.__getitem__(name)

    def __getitem__(self, name_or_ordinal):
        return self._Ptr((name_or_ordinal, self))


def lookup_by_element(name, lib):
    """Only element 0 of the tuple is the looked-up name (here: `lib`)."""
    return _FuncPtr((lib, name))


class Client:
    """An HTTP field is a prefix the sender extends, never a whole key: no sent field."""

    def __init__(self, base_url):
        self.base_url = base_url

    def get(self):
        _net.send(self.base_url)
