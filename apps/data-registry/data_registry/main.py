import logging
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI
from fastapi.staticfiles import StaticFiles

from .api import router
from .logging_config import configure_logging


@asynccontextmanager
async def lifespan(_: FastAPI):
    logging.getLogger("data_registry").info("registry_started")
    yield
    logging.getLogger("data_registry").info("registry_stopped")


def create_app() -> FastAPI:
    configure_logging()
    application = FastAPI(
        title="Icy Seas Data Registry",
        version="0.1.0",
        lifespan=lifespan,
    )
    application.include_router(router)
    application.mount(
        "/admin",
        StaticFiles(directory=Path(__file__).parent / "admin", html=True),
        name="admin-console",
    )
    return application


app = create_app()
