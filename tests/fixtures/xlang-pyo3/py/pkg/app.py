"""Fixture (P5, pyo3): Python callers of the `_native` extension module."""
from pkg import _native
from pkg.util import fast_sum as local_sum


def run():
    total = _native.fast_sum([1, 2, 3])  # unique -> proven
    one = _native.renamed()  # `#[pyo3(name = "renamed")]` -> proven
    counter = _native.Counter()  # class call runs `#[new]` -> proven
    two = _native.shared()  # module of `shared` unknown, two candidates -> possible
    _native.missing()  # negative control: not exported -> no bridge
    three = local_sum([4])  # negative control: pure Python -> no bridge
    return total + one + two + three, counter
