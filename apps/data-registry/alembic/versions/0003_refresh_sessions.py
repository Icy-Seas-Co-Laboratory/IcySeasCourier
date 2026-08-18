"""Renewable Courier sessions.

Revision ID: 0003
Revises: 0002
"""

import sqlalchemy as sa

from alembic import op

revision = "0003"
down_revision = "0002"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.add_column("courier_sessions", sa.Column("refresh_token_hash", sa.String(64)))
    op.add_column(
        "courier_sessions", sa.Column("refresh_expires_at", sa.DateTime(timezone=True))
    )
    op.add_column("courier_sessions", sa.Column("revoked_at", sa.DateTime(timezone=True)))
    op.create_index(
        "ix_courier_sessions_refresh_token_hash",
        "courier_sessions",
        ["refresh_token_hash"],
        unique=True,
    )


def downgrade() -> None:
    op.drop_index("ix_courier_sessions_refresh_token_hash", table_name="courier_sessions")
    op.drop_column("courier_sessions", "revoked_at")
    op.drop_column("courier_sessions", "refresh_expires_at")
    op.drop_column("courier_sessions", "refresh_token_hash")
