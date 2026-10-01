def compute(x):
    total = helper(x)
    return total + len(str(x))


def helper(x):
    return x * 2


print(compute(3))
