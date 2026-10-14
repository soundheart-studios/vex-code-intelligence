"""A hub many callers share, a call chain and a cycle."""


def hub(x):
    """The shared helper."""
    return x


def leaf():
    return 1


def mid():
    return leaf()


def top():
    return mid()


def ping(n):
    return pong(n - 1)


def pong(n):
    return ping(n)
