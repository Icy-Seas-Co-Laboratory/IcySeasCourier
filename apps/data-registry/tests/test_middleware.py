from data_registry.middleware import RateLimiter


def test_rate_limiter_enforces_a_sliding_window() -> None:
    limiter = RateLimiter(maximum_clients=10)

    assert not limiter.limited("auth", "127.0.0.1", maximum=2, now=100)
    assert not limiter.limited("auth", "127.0.0.1", maximum=2, now=101)
    assert limiter.limited("auth", "127.0.0.1", maximum=2, now=102)
    assert not limiter.limited("auth", "127.0.0.1", maximum=2, now=161)


def test_rate_limiter_bounds_tracked_clients() -> None:
    limiter = RateLimiter(maximum_clients=2)

    assert not limiter.limited("auth", "client-1", maximum=2, now=100)
    assert not limiter.limited("auth", "client-2", maximum=2, now=100)
    assert not limiter.limited("auth", "client-3", maximum=2, now=100)

    assert len(limiter.requests) == 2
    assert ("auth", "client-1") not in limiter.requests
