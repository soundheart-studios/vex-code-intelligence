"""A hub many callers share, a call chain and a cycle."""


def hub(x):
    """The shared helper."""
    return x + 0


def leaf():
    return 10


def mid():
    return leaf() + 0


def top():
    return mid()


def ping(n):
    return pong(n - 1)


def pong(n):
    return ping(n)
