"""Add durable administrator TOTP enrollment.

Revision ID: 0007
Revises: 0006
"""

from collections.abc import Sequence

import sqlalchemy as sa

from alembic import op

revision: str = "0007"
down_revision: str | None = "0006"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    op.create_table(
        "admin_security",
        sa.Column("id", sa.Integer(), primary_key=True),
        sa.Column("encrypted_totp_secret", sa.Text(), nullable=False),
        sa.Column("setup_expires_at", sa.DateTime(timezone=True)),
        sa.Column("configured_at", sa.DateTime(timezone=True)),
        sa.Column("last_used_totp_step", sa.BigInteger()),
    )


def downgrade() -> None:
    op.drop_table("admin_security")
