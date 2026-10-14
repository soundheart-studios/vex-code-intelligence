"""Calls resolved only by name: no receiver type behind them."""


class Alpha:
    def process(self):
        return 1


class Beta:
    def process(self):
        return 2


def run_any(obj):
    return obj.process()


class Gamma(Alpha):
    def go(self):
        return self.process()
