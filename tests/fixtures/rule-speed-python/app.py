from store import Store, make_store
from util import helper


def main():
    s = make_store()
    s.save()
    helper()
    return run()


def run():
    return helper()
