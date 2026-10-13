"""Idempotency records for durable Courier client logs."""

import sqlalchemy as sa

from alembic import op

revision = "0010"
down_revision = "0009"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.create_table(
        "transfer_client_events",
        sa.Column("id", sa.Uuid(), primary_key=True),
        sa.Column(
            "transfer_id",
            sa.Uuid(),
            sa.ForeignKey("transfers.id", ondelete="CASCADE"),
            nullable=False,
        ),
        sa.Column("payload_sha256", sa.String(64), nullable=False),
    )


def downgrade() -> None:
    op.drop_table("transfer_client_events")
