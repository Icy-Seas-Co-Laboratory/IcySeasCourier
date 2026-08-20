import logging
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI
from fastapi.responses import FileResponse
from fastapi.staticfiles import StaticFiles

from .api import router
from .config import Settings, get_settings
from .logging_config import configure_logging
from .middleware import SecurityMiddleware


@asynccontextmanager
async def lifespan(_: FastAPI):
    logging.getLogger("data_registry").info("registry_started")
    yield
    logging.getLogger("data_registry").info("registry_stopped")


def create_app(settings: Settings | None = None) -> FastAPI:
    configure_logging()
    settings = settings or get_settings()
    application = FastAPI(
        title="Icy Seas Data Registry",
        version="0.1.0",
        lifespan=lifespan,
        docs_url=None if settings.environment != "development" else "/docs",
        redoc_url=None if settings.environment != "development" else "/redoc",
        openapi_url=None if settings.environment != "development" else "/openapi.json",
    )
    application.add_middleware(SecurityMiddleware, settings=settings)

    @application.get("/", include_in_schema=False)
    def landing_page() -> FileResponse:
        return FileResponse(
            Path(__file__).parent / "landing" / "index.html",
            media_type="text/html",
            headers={
                "Cache-Control": "public, max-age=300",
                "Content-Security-Policy": (
                    "default-src 'none'; style-src 'unsafe-inline'; "
                    "img-src 'self'; frame-ancestors 'none'; base-uri 'none'"
                ),
            },
        )

    @application.get("/favicon.svg", include_in_schema=False)
    def favicon() -> FileResponse:
        return FileResponse(
            Path(__file__).parent / "landing" / "favicon.svg",
            media_type="image/svg+xml",
            headers={"Cache-Control": "public, max-age=86400"},
        )

    application.include_router(router)
    application.mount(
        "/admin",
        StaticFiles(directory=Path(__file__).parent / "admin", html=True),
        name="admin-console",
    )
    return application


app = create_app()
