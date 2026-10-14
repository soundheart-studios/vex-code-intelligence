"""Every shape a symbol's change can take."""


def keep(x):
    return x


def mover(a):
    if a:
        return 1
    return 0


def editbody(a):
    return a + 1


def editsig(a):
    return a * 2


def gone():
    return None


class A:
    def run(self):
        return 1


class B:
    def run(self):
        return 2


def caller():
    return keep(1) + mover(2) + editbody(3) + editsig(4)
