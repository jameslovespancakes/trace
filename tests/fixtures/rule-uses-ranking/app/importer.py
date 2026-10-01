from core.worker import Worker


def make():
    return Worker()


def imp_a(w):
    return w.process("imp-a")


def imp_b(w):
    return w.process("imp-b")


def imp_c(w):
    return w.process("imp-c")


def imp_d(w):
    return w.process("imp-d")


def imp_e(w):
    return w.process("imp-e")


def imp_f(w):
    return w.process("imp-f")


def imp_g(w):
    return w.process("imp-g")


def imp_h(w):
    return w.process("imp-h")


def imp_i(w):
    return w.process("imp-i")
