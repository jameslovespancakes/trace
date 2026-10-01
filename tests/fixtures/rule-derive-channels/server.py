"""A tiny web + messaging library (neutral names): registrations, mounts, decorators, sends."""
import _net
import _proc
import _loop


class Route:
    def __init__(self, path, endpoint, name=None):
        self.path = path
        self.endpoint = endpoint
        self.name = name

    def handle(self, scope, receive, send):
        return self.endpoint(scope)


class Router:
    def __init__(self):
        self.routes = []

    def add_route(self, path, endpoint):
        self.routes.append(Route(path, endpoint))

    def route(self, path):
        def decorator(func):
            self.add_route(path, func)
            return func
        return decorator

    def mount(self, prefix, router):
        for r in router.routes:
            self.add_route(prefix + r.path, r.endpoint)

    def __call__(self, scope, receive, send):
        for r in self.routes:
            if r.path == scope:
                return r.handle(scope, receive, send)


class Store:
    """Keeps callables but never dispatches them from an entry."""

    def __init__(self):
        self.items = {}

    def keep(self, key, fn):
        self.items[key] = fn


def get(url, params=None):
    return request("GET", url, params)


def request(method, url, params=None):
    return _net.send(method, url)


def run_command(cmd):
    return _proc.spawn(cmd)


class Consumer:
    def __init__(self):
        self.handlers = {}

    def subscribe(self, topic, handler):
        self.handlers[topic] = handler

    def _dispatch(self, message):
        self.handlers[message](message)

    def start(self):
        _loop.run(self._dispatch)
