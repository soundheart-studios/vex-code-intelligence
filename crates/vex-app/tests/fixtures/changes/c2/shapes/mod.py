"""Every shape a symbol's change can take."""


def keep(x):
    return x


def editbody(a):
    if a > 10:
        return a
    return a + 2


def editsig(a, b=1):
    return a * 2


class A:
    def run(self):
        return 1


class B:
    def run(self):
        return 20


class C:
    def run(self):
        return 2


def fresh():
    return "new"


def caller():
    return keep(1) + mover(2) + editbody(3) + editsig(4)


def mover(a):
    if a:
        return 1
    return 0
