from fastapi import Request

from data_registry.config import Settings
from data_registry.middleware import RateLimiter, SecurityMiddleware


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


def test_trusted_cloudflare_proxy_prefers_connecting_ip_over_spoofable_xff() -> None:
    middleware = SecurityMiddleware(
        app=lambda scope, receive, send: None,
        settings=Settings(trusted_proxy_networks="172.30.50.10/32"),
    )
    request = Request(
        {
            "type": "http",
            "method": "GET",
            "path": "/",
            "headers": [
                (b"cf-connecting-ip", b"203.0.113.42"),
                (b"x-forwarded-for", b"127.0.0.1, 203.0.113.42"),
            ],
            "client": ("172.30.50.10", 50000),
            "scheme": "https",
            "server": ("courier.icyseascolab.io", 443),
        }
    )

    assert str(middleware._address(request)) == "203.0.113.42"


def test_untrusted_peer_cannot_supply_cloudflare_connecting_ip() -> None:
    middleware = SecurityMiddleware(
        app=lambda scope, receive, send: None,
        settings=Settings(trusted_proxy_networks="172.30.50.10/32"),
    )
    request = Request(
        {
            "type": "http",
            "method": "GET",
            "path": "/",
            "headers": [(b"cf-connecting-ip", b"127.0.0.1")],
            "client": ("192.0.2.20", 50000),
            "scheme": "https",
            "server": ("courier.icyseascolab.io", 443),
        }
    )

    assert str(middleware._address(request)) == "192.0.2.20"
