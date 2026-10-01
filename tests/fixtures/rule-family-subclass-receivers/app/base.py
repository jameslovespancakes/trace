"""Routing base class and two applications."""


class Base:
    """Registers routes."""

    def __init__(self):
        self.routes = {}

    def get(self, rule):
        """Register a GET route."""
        return self.route("GET", rule)

    def route(self, method, rule):
        def decorator(view):
            self.routes[(method, rule)] = view
            return view

        return decorator


class App(Base):
    """An application that inherits `get`."""


class LoggingApp(Base):
    """An application that logs route registration."""

    def get(self, rule):
        print("GET", rule)
        return super().get(rule)
