"""Module-level code, a write, imports, an override pair and lambda callbacks."""
from shop.pricing import subtotal


class Renderer:
    def render(self, prices: list[float]) -> str:
        return str(subtotal(prices))


class PlainRenderer(Renderer):
    def render(self, prices: list[float]) -> str:
        return "plain"


class Hooks:
    def redirect(self, location: str) -> str:
        return location


def show(renderer: Renderer, prices: list[float]) -> str:
    return renderer.render(prices)


def install(hooks: Hooks) -> None:
    hooks.redirect = lambda location: location.upper()


def register(callback):
    return callback


register(lambda prices: subtotal(prices))
DEFAULT_TOTAL = subtotal([1.0, 2.0])
PLAIN = PlainRenderer().render([3.0])
