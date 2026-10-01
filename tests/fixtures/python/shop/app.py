"""Entry point wiring the fixture together."""
from shop.pricing import MinimumRule, PercentOff, apply_discount, convert, subtotal, validate
from shop.store import Inventory, report


def checkout(prices: list[float]) -> float:
    amount = subtotal(prices)
    if not validate(MinimumRule(), amount):
        return 0.0
    return apply_discount(PercentOff(10), amount)


def main() -> str:
    inventory = Inventory()
    inventory.restock("pear", 2.0)
    total = checkout([1.0, 2.5, 30.0])
    return report(inventory) + str(total)


async def main_async() -> float:
    return await convert(checkout([5.0]), "EUR")
