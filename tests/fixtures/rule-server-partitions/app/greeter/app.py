from greeter.format import format_greeting
from greeter.io_util import write_line


def main(args):
    name = args[0] if args else "world"
    print(format_greeting(name))
    write_line(format_greeting(name.upper()))
