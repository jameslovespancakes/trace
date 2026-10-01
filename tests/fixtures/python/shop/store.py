"""Descriptors, subscript dunders, decorators and callbacks."""
from shop.fmt import format_price


class cached:
    """Non-data descriptor caching a computed attribute."""

    def __init__(self, func):
        self.func = func

    def __get__(self, obj, objtype=None):
        if obj is None:
            return self
        value = obj.__dict__[self.func.__name__] = self.func(obj)
        return value


def logged(func):
    def wrapper(*args, **kwargs):
        return func(*args, **kwargs)

    return wrapper


class Inventory:
    def __init__(self) -> None:
        self._items: dict[str, float] = {}

    def __getitem__(self, key: str) -> float:
        return self._items[key]

    def __setitem__(self, key: str, value: float) -> None:
        self._items[key] = value

    @cached
    def total(self) -> float:
        return sum(self._items.values())

    @logged
    def restock(self, key: str, value: float) -> None:
        self[key] = value


def on_change(key: str) -> None:
    print(format_price(len(key)))


def notify_all(keys: list[str], callback=on_change) -> None:
    for key in keys:
        callback(key)


def report(inventory: Inventory) -> str:
    inventory["apple"] = 1.5
    price = inventory["apple"]
    notify_all(["apple"], on_change)
    return format_price(price + inventory.total)
