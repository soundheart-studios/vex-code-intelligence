"""Definitions that walk every rung of expand's detail ladder."""


def one_liner(x):
    """One line doc."""
    return x


def multi_doc(a,
              b):
    """First line.

    More text.
    """
    return a + b


def single_quotes():
    '''Single-quoted doc
    over two lines.'''
    return 1


def no_doc(value):
    return value * 2


def unclosed_doc():
    """This docstring never closes on its own line
    because the closing quotes come on a line
    that also ends the body""" ; return 3


def long_signature(
    alpha,
    beta,
    gamma,
    delta,
    epsilon,
    zeta,
    eta,
    theta,
    iota,
    kappa,
    lam,
    mu,
):
    return alpha


def salutation():
    """Grüße, naïve café — 日本 😀."""
    return "ß"


class Box:
    """A box."""

    def __init__(self):
        self._v = 0

    @property
    def value(self):
        """Getter."""
        return self._v

    @value.setter
    def value(self, v):
        self._v = v
