"""Structured pipeline metrics (design §27).

A Metrics accumulator holds counts per pipeline stage and derived
rates. The no-sensitive-content rule is structural: the accumulator
only ever stores numbers under fixed names — no question, answer,
fact statement or KO text can land in a metric — so `as_dict()` is
safe to emit, file and report verbatim.
"""

from __future__ import annotations


class Metrics:
    def __init__(self):
        self._counts = {}
        self._rates = []  # (name, numerator_count, denominator_count)

    def count(self, name, n=1):
        self._counts[name] = self._counts.get(name, 0) + n

    def rate(self, name, numerator, denominator):
        self._rates.append((name, numerator, denominator))

    def as_dict(self):
        counts = dict(self._counts)
        rates = {}
        for name, num, den in self._rates:
            if counts.get(den):
                rates[name] = round(counts.get(num, 0) / counts[den], 4)
            else:
                rates[name] = None  # undefined, not zero
        return {"counts": counts, "rates": rates}
