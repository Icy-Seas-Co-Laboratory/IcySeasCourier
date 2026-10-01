FROM python:3.12-slim

ARG REGISTRY_BUILD_ID=local

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    REGISTRY_BUILD_ID=${REGISTRY_BUILD_ID}

WORKDIR /app
COPY pyproject.toml uv.lock alembic.ini ./
COPY alembic ./alembic
COPY data_registry ./data_registry
COPY scripts ./scripts
RUN pip install --no-cache-dir .

EXPOSE 8010
CMD ["sh", "-c", "alembic upgrade head && uvicorn data_registry.main:app --host 0.0.0.0 --port 8010"]
