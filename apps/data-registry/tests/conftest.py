import io
from collections.abc import Generator

import pytest
from fastapi.testclient import TestClient
from pydantic import SecretStr
from sqlalchemy import create_engine
from sqlalchemy.orm import Session, sessionmaker
from sqlalchemy.pool import StaticPool

from data_registry import models  # noqa: F401
from data_registry.config import Settings, get_settings
from data_registry.db import Base, get_session
from data_registry.main import create_app
from data_registry.storage import get_object_storage


class FakeObjectStorage:
    url_lifetime = 900

    def __init__(self) -> None:
        self.uploads: dict[str, str] = {}
        self.remote_parts: list[dict] = []
        self.objects: dict[str, bytes] = {}

    def create_multipart(self, object_key: str) -> str:
        upload_id = f"upload-{len(self.uploads) + 1}"
        self.uploads[object_key] = upload_id
        return upload_id

    def list_parts(self, object_key: str, upload_id: str) -> list[dict]:
        assert self.uploads[object_key] == upload_id
        return self.remote_parts

    def authorize_part(self, object_key: str, upload_id: str, part_number: int) -> str:
        assert self.uploads[object_key] == upload_id
        return f"https://objects.test/{object_key}?uploadId={upload_id}&partNumber={part_number}"

    def authorize_download(self, object_key: str) -> str:
        return f"https://objects.test/{object_key}?download=1"

    def complete_multipart(self, object_key: str, upload_id: str, parts: list[dict]) -> str:
        assert self.uploads[object_key] == upload_id
        assert parts
        return '"completed-etag"'

    def abort_multipart(self, object_key: str, upload_id: str) -> None:
        assert self.uploads.get(object_key) == upload_id
        self.uploads.pop(object_key, None)

    def delete_object(self, object_key: str) -> None:
        self.objects.pop(object_key, None)

    def open_object(self, object_key: str):
        return io.BytesIO(self.objects[object_key])

    def object_size(self, object_key: str) -> int:
        return len(self.objects[object_key])

    def object_exists(self, object_key: str) -> bool:
        return object_key in self.objects


@pytest.fixture
def fake_storage() -> FakeObjectStorage:
    return FakeObjectStorage()


@pytest.fixture
def settings() -> Settings:
    return Settings(
        database_url="sqlite+pysqlite://",
        admin_api_key=SecretStr("test-admin-key"),
        token_pepper=SecretStr("test-token-pepper"),
        session_lifetime_seconds=3600,
    )


@pytest.fixture
def database_factory() -> Generator[sessionmaker[Session]]:
    engine = create_engine(
        "sqlite+pysqlite://",
        connect_args={"check_same_thread": False},
        poolclass=StaticPool,
    )
    Base.metadata.create_all(engine)
    factory = sessionmaker(bind=engine, expire_on_commit=False)
    yield factory
    Base.metadata.drop_all(engine)
    engine.dispose()


@pytest.fixture
def client(
    settings: Settings,
    database_factory: sessionmaker[Session],
    fake_storage: FakeObjectStorage,
) -> Generator[TestClient]:
    app = create_app(settings)

    def override_session():
        with database_factory() as database:
            yield database

    app.dependency_overrides[get_session] = override_session
    app.dependency_overrides[get_settings] = lambda: settings
    app.dependency_overrides[get_object_storage] = lambda: fake_storage
    with TestClient(app, client=("127.0.0.1", 50000)) as test_client:
        yield test_client
