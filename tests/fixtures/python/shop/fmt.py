def format_price(value):
    return "$" + _two_places(value)


def _two_places(value):
    return "%.2f" % value
