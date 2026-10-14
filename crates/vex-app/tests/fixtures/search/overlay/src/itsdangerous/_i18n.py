"""Grüße: a signer greeting with non-ASCII text."""


def greet_signer(name: str) -> str:
    """Return a greeting for the signer — ünicode included."""
    return f"Grüße, {name}! ✓ signed"
