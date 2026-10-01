"""A tiny web library (neutral names) whose route objects hand the endpoint to the entry through
wrappers, records and typed parameters (derive_params / derive_methods / derive_tables rules)."""


class Record:
    def __init__(self, path=None, call=None):
        self.path = path
        self.call = call


def describe(path, call):
    return Record(path=path, call=call)


def run_record(record: Record, request):
    return record.call(request)


def make_handler(call):
    def handler(request):
        return call(request)
    return handler


def wrap(func):
    def app(scope, receive, send):
        return func(scope)
    return app


class WrappedRoute:
    """The endpoint is called only through a wrapper closure built at registration time."""

    def __init__(self, path, endpoint):
        self.path = path
        self.endpoint = endpoint
        self.app = wrap(make_handler(self.endpoint))

    def matches(self, scope):
        return scope

    def handle(self, scope, receive, send):
        return self.app(scope, receive, send)


class RecordRoute:
    """The endpoint reaches the called slot through a function storing its parameter."""

    def __init__(self, path, endpoint):
        self.path = path
        self.endpoint = endpoint
        self.record = describe(path, call=self.endpoint)

    def matches(self, scope):
        return scope

    def handle(self, scope, receive, send):
        return run_record(self.record, scope)


class GroupRoute:
    """Runs its endpoint, a mounted router's app and a model's hook (a typed parameter whose
    class owns no registry is no mount target)."""

    def __init__(self, path, endpoint, router, model: Record = None):
        self.path = path
        self.handlers = [endpoint, router.app, model.call]

    def matches(self, scope):
        return scope

    def handle(self, scope, receive, send):
        for h in self.handlers:
            h(scope)


class Router:
    def __init__(self, routes=None, lifespan=None, default=None):
        self.routes = [] if routes is None else list(routes)
        self.lifespan = lifespan
        self.default = default

    def add_wrapped(self, path, endpoint):
        self.routes.append(WrappedRoute(path, endpoint))

    def add_recorded(self, path, endpoint):
        self.routes.append(RecordRoute(path, endpoint))

    def add_group(self, path, endpoint, router, model: Record = None):
        self.routes.append(GroupRoute(path, endpoint, router, model))

    def get(self, path):
        def decorator(func):
            self.add_wrapped(path, func)
            return func
        return decorator

    def __call__(self, scope, receive, send):
        if scope == "lifespan":
            return self.lifespan(scope)
        for r in self.routes:
            if r.matches(scope):
                return r.handle(scope, receive, send)
        return self.default(scope)


class App:
    def __init__(self):
        self.router = Router()

    def get(self, path):
        return self.router.get(path)
