"""Fixture (P5, cpython): Python callers of C extension modules."""
import eggs
import spam


def main():
    spam.system("ls")  # unique table entry of module `spam` -> proven
    spam.shared()  # two tables without a module name export `shared` -> possible
    spam.nothing()  # negative control: not exported
    eggs.lay()  # negative control: `eggs` has Python source (py/eggs.py)
