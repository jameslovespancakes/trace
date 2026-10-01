"""Pricing rules: protocols, subclasses, generators and async code."""
import abc
from typing import Iterator, Protocol


class Discount(Protocol):
    def apply(self, amount: float) -> float: ...


class PercentOff:
    def __init__(self, percent: float) -> None:
        self.percent = percent

    def apply(self, amount: float) -> float:
        return amount * (1 - self.percent / 100)


class FlatOff:
    def __init__(self, value: float) -> None:
        self.value = value

    def apply(self, amount: float) -> float:
        return max(0.0, amount - self.value)


class Rule(abc.ABC):
    @abc.abstractmethod
    def check(self, amount: float) -> bool:
        """Return True when the rule accepts the amount."""

    def describe(self) -> str:
        return type(self).__name__


class MinimumRule(Rule):
    def check(self, amount: float) -> bool:
        return amount >= 10


class MaximumRule(Rule):
    def check(self, amount: float) -> bool:
        return amount <= 1000


def apply_discount(discount: Discount, amount: float) -> float:
    return discount.apply(amount)


def validate(rule: Rule, amount: float) -> bool:
    return rule.check(amount)


def line_totals(prices: list[float]) -> Iterator[float]:
    for price in prices:
        yield round(price, 2)


def subtotal(prices: list[float]) -> float:
    total = 0.0
    for value in line_totals(prices):
        total += value
    return total


async def fetch_rate(currency: str) -> float:
    return 1.0 if currency == "USD" else 0.9


async def convert(amount: float, currency: str) -> float:
    rate = await fetch_rate(currency)
    return amount * rate


def lazy_totals(prices: list[float]) -> Iterator[float]:
    return line_totals(prices)
