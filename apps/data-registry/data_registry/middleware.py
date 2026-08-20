import ipaddress
import time
from collections import OrderedDict, deque

from fastapi import Request
from starlette.middleware.base import BaseHTTPMiddleware, RequestResponseEndpoint
from starlette.responses import JSONResponse, Response

from .config import Settings


def _networks(value: str) -> tuple[ipaddress.IPv4Network | ipaddress.IPv6Network, ...]:
    return tuple(
        ipaddress.ip_network(item.strip(), strict=False)
        for item in value.split(",")
        if item.strip()
    )


class RateLimiter:
    """Bounded, process-local sliding-window limiter for a small Registry."""

    def __init__(self, maximum_clients: int) -> None:
        if maximum_clients < 1:
            raise ValueError("rate_limit_maximum_clients must be positive")
        self.maximum_clients = maximum_clients
        self.requests: OrderedDict[tuple[str, str], deque[float]] = OrderedDict()

    def limited(self, bucket: str, address: str, maximum: int, now: float | None = None) -> bool:
        now = time.monotonic() if now is None else now
        key = (bucket, address)
        history = self.requests.get(key)
        if history is None:
            if len(self.requests) >= self.maximum_clients:
                self.requests.popitem(last=False)
            history = deque()
            self.requests[key] = history
        else:
            self.requests.move_to_end(key)
        while history and history[0] <= now - 60:
            history.popleft()
        if len(history) >= maximum:
            return True
        history.append(now)
        return False


class SecurityMiddleware(BaseHTTPMiddleware):
    def __init__(self, app, settings: Settings) -> None:
        super().__init__(app)
        self.settings = settings
        self.admin_networks = _networks(settings.admin_allowed_networks)
        self.trusted_proxies = _networks(settings.trusted_proxy_networks)
        self.rate_limiter = RateLimiter(settings.rate_limit_maximum_clients)

    def _address(self, request: Request) -> ipaddress.IPv4Address | ipaddress.IPv6Address | None:
        host = request.client.host if request.client else ""
        try:
            peer = ipaddress.ip_address(host)
            if isinstance(peer, ipaddress.IPv6Address) and peer.ipv4_mapped:
                peer = peer.ipv4_mapped
        except ValueError:
            return None
        if any(peer in network for network in self.trusted_proxies):
            # Cloudflare supplies a single, normalized client address here. Prefer it
            # over X-Forwarded-For, whose leftmost value may have existed before the
            # request reached Cloudflare. Other trusted proxies fall back to XFF.
            forwarded = request.headers.get("cf-connecting-ip", "").strip()
            if not forwarded:
                forwarded = request.headers.get("x-forwarded-for", "").split(",", 1)[0].strip()
            if forwarded:
                try:
                    forwarded_address = ipaddress.ip_address(forwarded)
                    if (
                        isinstance(forwarded_address, ipaddress.IPv6Address)
                        and forwarded_address.ipv4_mapped
                    ):
                        return forwarded_address.ipv4_mapped
                    return forwarded_address
                except ValueError:
                    return None
        return peer

    async def dispatch(self, request: Request, call_next: RequestResponseEndpoint) -> Response:
        address = self._address(request)
        path = request.url.path
        is_admin = (
            path == "/admin" or path.startswith("/admin/") or path.startswith("/api/v1/admin")
        )
        if is_admin and (
            address is None or not any(address in network for network in self.admin_networks)
        ):
            return JSONResponse({"detail": "admin access is restricted"}, status_code=403)

        if self.settings.require_https and request.url.scheme != "https":
            return JSONResponse({"detail": "HTTPS is required"}, status_code=400)

        content_length = request.headers.get("content-length")
        if content_length:
            try:
                too_large = int(content_length) > self.settings.maximum_request_body_bytes
            except ValueError:
                return JSONResponse({"detail": "invalid content length"}, status_code=400)
            if too_large:
                return JSONResponse({"detail": "request body is too large"}, status_code=413)
        if request.method in {"POST", "PUT", "PATCH"}:
            chunks = []
            received = 0
            async for chunk in request.stream():
                received += len(chunk)
                if received > self.settings.maximum_request_body_bytes:
                    return JSONResponse({"detail": "request body is too large"}, status_code=413)
                chunks.append(chunk)
            request._body = b"".join(chunks)  # Starlette replays this body to the route.

        rate_limited = False
        if path in {"/api/v1/auth/invitations/exchange", "/api/v1/auth/sessions/refresh"}:
            rate_limited = self.rate_limiter.limited(
                "authentication", str(address), self.settings.authentication_requests_per_minute
            )
        elif is_admin:
            rate_limited = self.rate_limiter.limited(
                "administration", str(address), self.settings.admin_requests_per_minute
            )
        elif path.startswith("/api/v1/"):
            rate_limited = self.rate_limiter.limited(
                "client", str(address), self.settings.client_requests_per_minute
            )
        if rate_limited:
            return JSONResponse(
                {"detail": "rate limit exceeded"},
                status_code=429,
                headers={"Retry-After": "60"},
            )

        response = await call_next(request)
        response.headers["X-Content-Type-Options"] = "nosniff"
        response.headers["X-Frame-Options"] = "DENY"
        response.headers["Referrer-Policy"] = "no-referrer"
        response.headers["Permissions-Policy"] = "camera=(), microphone=(), geolocation=()"
        if request.url.scheme == "https":
            response.headers["Strict-Transport-Security"] = "max-age=31536000"
        if is_admin:
            response.headers["Cache-Control"] = "no-store"
            response.headers["Content-Security-Policy"] = (
                "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; "
                "img-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'"
            )
        return response
