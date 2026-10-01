"""Library source for the derivation rules (installed-library shapes, neutral names)."""
import _native


def run(fn, value):
    return fn(value)


def alias_call(fn):
    g = fn
    g()


def identity(fn):
    return fn


def wrap(fn):
    def inner(*args):
        return fn(*args)
    return inner


def total(items):
    s = 0
    for x in items:
        s = s + x
    return s


def via_native(fn, data):
    return _native.apply(fn, data)


def forward(fn):
    return run(fn, 1)


class Worker:
    def __init__(self, target, name=None, args=()):
        self._target = target
        self._name = name
        self._args = args

    def start(self):
        self._target(*self._args)


class Box:
    def put(self, other, fn):
        other.callback = fn

    def fire(self, other):
        other.callback()


class lazy:
    def __init__(self, func):
        self.func = func

    def __get__(self, instance, owner=None):
        return self.func(instance)


class Runner:
    def submit(self, fn):
        fn()


def schedule(executor, fn):
    executor.submit(fn)


def _make_wrapper(user_function):
    def wrapper(*args, **kwds):
        return user_function(*args, **kwds)
    return wrapper


def lru(maxsize=128):
    def decorating_function(user_function):
        wrapper = _make_wrapper(user_function)
        return wrapper
    return decorating_function


def cache(user_function):
    return lru(maxsize=None)(user_function)


class partialmethod:
    def __init__(self, func, *args):
        self.func = func
        self.args = args

    def _make_unbound_method(self):
        def _method(cls_or_self, *args):
            return self.func(cls_or_self, *self.args, *args)
        return _method


def dispatcher(func):
    registry = {}

    def dispatch(cls):
        return registry[cls]

    def register(cls, impl=None):
        registry[cls] = impl
        return impl

    def wrapper(*args):
        return dispatch(args[0])(*args)

    registry[object] = func
    wrapper.register = register
    return wrapper


class finalizer:
    _registry = {}

    class _Info:
        pass

    def __init__(self, obj, func, *args):
        info = self._Info()
        info.func = func
        info.args = args
        self._registry[self] = info

    def __call__(self, _=None):
        info = self._registry.pop(self, None)
        if info:
            return info.func(*info.args)


class Emitter:
    def __init__(self):
        self._handlers = []

    def on(self, handler):
        self._handlers.append(handler)

    def emit(self, event):
        for h in self._handlers:
            h(event)
