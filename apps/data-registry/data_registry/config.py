from functools import lru_cache

from pydantic import SecretStr, model_validator
from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    model_config = SettingsConfigDict(env_prefix="REGISTRY_", env_file=".env", extra="ignore")

    environment: str = "development"
    database_url: str = "postgresql+psycopg://registry:registry@127.0.0.1:5432/registry"
    admin_api_key: SecretStr = SecretStr("development-only-change-me")
    token_pepper: SecretStr = SecretStr("development-only-change-me-too")
    session_lifetime_seconds: int = 3600
    refresh_lifetime_seconds: int = 2_592_000
    s3_internal_endpoint_url: str = "http://127.0.0.1:8333"
    s3_public_endpoint_url: str = "http://127.0.0.1:8333"
    s3_region: str = "us-east-1"
    s3_bucket: str = "icy-seas-incoming"
    s3_access_key_id: SecretStr = SecretStr("development")
    s3_secret_access_key: SecretStr = SecretStr("development")
    upload_url_lifetime_seconds: int = 900
    verification_poll_seconds: float = 1.0
    verification_lease_seconds: int = 300
    verification_max_attempts: int = 3
    hash_algorithm: str = "sha256"

    @model_validator(mode="after")
    def reject_development_secrets_outside_development(self) -> "Settings":
        self.hash_algorithm = self.hash_algorithm.lower()
        if self.hash_algorithm not in {"sha256", "xxhash3", "blake3"}:
            raise ValueError("hash_algorithm must be sha256, xxhash3, or blake3")
        if self.environment != "development":
            defaults = {"development-only-change-me", "development-only-change-me-too"}
            if (
                self.admin_api_key.get_secret_value() in defaults
                or self.token_pepper.get_secret_value() in defaults
            ):
                raise ValueError("development secrets are forbidden outside development")
        return self


@lru_cache
def get_settings() -> Settings:
    return Settings()
