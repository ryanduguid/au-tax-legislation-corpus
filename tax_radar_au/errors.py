class MonitorError(ValueError):
    """A source, observation, mapping, or decision is outside the v0.1 contract."""


class SourceTooLargeError(MonitorError):
    """A source exceeds the caller's byte limit."""
