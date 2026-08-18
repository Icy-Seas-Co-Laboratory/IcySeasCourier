FROM python:3.12-slim

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1

WORKDIR /app
COPY pyproject.toml uv.lock alembic.ini ./
COPY alembic ./alembic
COPY data_registry ./data_registry
COPY scripts ./scripts
RUN pip install --no-cache-dir .

EXPOSE 8000
CMD ["sh", "-c", "alembic upgrade head && uvicorn data_registry.main:app --host 0.0.0.0 --port 8000"]
