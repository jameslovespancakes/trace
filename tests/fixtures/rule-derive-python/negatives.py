"""Data parameters that are never called (the lab prototype's 30 negatives, as library shapes)."""


class Queue:
    def __init__(self, maxsize=0):
        self.maxsize = maxsize
        self.queue = []

    def put(self, item, block=True, timeout=None):
        if timeout is not None and timeout < 0:
            raise ValueError("timeout must be a non-negative number")
        self._put(item)

    def put_nowait(self, item):
        return self.put(item, block=False)

    def _put(self, item):
        self.queue.append(item)

    def get(self):
        return self.queue.pop()


class Thread:
    def __init__(self, group=None, target=None, name=None, args=(), kwargs=None):
        self._target = target
        self._name = str(name)
        self._args = args
        self._kwargs = kwargs

    def run(self):
        if self._target is not None:
            self._target(*self._args, **self._kwargs)


class Timer(Thread):
    def __init__(self, interval, function, args=None, kwargs=None):
        Thread.__init__(self)
        self.interval = interval
        self.function = function
        self.args = args

    def run(self):
        self.function(*self.args)


class Event:
    def wait(self, timeout=None):
        return self._cond_wait(timeout)

    def _cond_wait(self, timeout):
        return timeout is None


def sleep(delay, result=None):
    if delay <= 0:
        return result
    return result


class Future:
    def set_result(self, result):
        self._result = result

    def result(self):
        return self._result


class Handle:
    def __init__(self, callback, args):
        self._callback = callback
        self._args = args


class TimerHandle(Handle):
    def __init__(self, when, callback, args):
        super().__init__(callback, args)
        self._when = when


class BaseEventLoop:
    def call_later(self, delay, callback, *args):
        return self.call_at(self.time() + delay, callback, *args)

    def call_at(self, when, callback, *args):
        return TimerHandle(when, callback, args)

    def time(self):
        return 0


class ThreadPoolExecutor:
    def __init__(self, max_workers=None, initializer=None):
        self._max_workers = max_workers
        self._initializer = initializer


class Counter(dict):
    def __init__(self, iterable=None, **kwds):
        self.update(iterable, **kwds)

    def update(self, iterable=None, **kwds):
        if iterable is not None:
            for elem in iterable:
                self[elem] = self.get(elem, 0) + 1


class OrderedDict(dict):
    def __init__(self, other=(), **kwds):
        self._data = {}
        for key in other:
            self._data[key] = other[key]


class partial:
    def __new__(cls, func, *args, **keywords):
        self = super().__new__(cls)
        self.func = func
        self.args = args
        return self

    def __call__(self, *args, **keywords):
        return self.func(*self.args, *args, **keywords)


class ExitStack:
    def __init__(self):
        self._exit_callbacks = []

    def callback(self, callback, *args, **kwds):
        def _exit_wrapper(exc_type, exc, tb):
            callback(*args, **kwds)
        self._exit_callbacks.append(_exit_wrapper)
        return callback

    def close(self):
        while self._exit_callbacks:
            cb = self._exit_callbacks.pop()
            cb(None, None, None)


class scheduler:
    def __init__(self):
        self._queue = []

    def enter(self, delay, priority, action, argument=(), kwargs=None):
        return self.enterabs(delay, priority, action, argument, kwargs)

    def enterabs(self, time, priority, action, argument=(), kwargs=None):
        event = (time, priority, action, argument, kwargs)
        self._queue.append(event)
        return event


class Response:
    def __init__(self, content=None, status_code=200, headers=None):
        self.status_code = status_code
        self.body = self.render(content)
        self.headers = headers

    def render(self, content):
        if content is None:
            return b""
        return content.encode("utf-8")


class JSONResponse(Response):
    def render(self, content):
        return str(content).encode("utf-8")


class Headers:
    def __init__(self, headers=None):
        self._list = [(k, v) for k, v in (headers or {}).items()]


def redirect(location, code=302):
    response = Response(content=location, status_code=code)
    response.headers = {"Location": location}
    return response


def dumps(obj, **kwargs):
    return str(obj)


class App:
    def make_response(self, rv):
        if isinstance(rv, tuple):
            rv = rv[0]
        return Response(rv)


def checkpoint(x=None):
    return x
