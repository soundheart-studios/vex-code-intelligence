from mappkg.many import alpha, beta, kappa
from mappkg.ünïcode import größe, Straße


def consume():
    return alpha() + beta() + kappa() + größe(3) + Straße().länge()
