import logging
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI
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
    application.include_router(router)
    application.mount(
        "/admin",
        StaticFiles(directory=Path(__file__).parent / "admin", html=True),
        name="admin-console",
    )
    return application


app = create_app()
