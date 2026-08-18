"""Independent transfer verification evidence.

Revision ID: 0004
Revises: 0003
"""

import sqlalchemy as sa

from alembic import op

revision = "0004"
down_revision = "0003"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.add_column("transfers", sa.Column("verification_started_at", sa.DateTime(timezone=True)))
    op.add_column(
        "transfers",
        sa.Column("verification_attempt_count", sa.Integer(), nullable=False, server_default="0"),
    )
    op.add_column("transfers", sa.Column("verification_error", sa.Text()))
    op.add_column("transfer_files", sa.Column("verified_size", sa.BigInteger()))
    op.add_column("transfer_files", sa.Column("verified_sha256", sa.String(64)))
    op.add_column("transfer_files", sa.Column("verified_at", sa.DateTime(timezone=True)))
    op.add_column("transfer_files", sa.Column("verification_error", sa.Text()))
    op.create_table(
        "verification_attempts",
        sa.Column("id", sa.Uuid(), primary_key=True),
        sa.Column(
            "transfer_id",
            sa.Uuid(),
            sa.ForeignKey("transfers.id", ondelete="CASCADE"),
            nullable=False,
        ),
        sa.Column("attempt_number", sa.Integer(), nullable=False),
        sa.Column("status", sa.String(30), nullable=False),
        sa.Column("started_at", sa.DateTime(timezone=True), server_default=sa.func.now()),
        sa.Column("completed_at", sa.DateTime(timezone=True)),
        sa.Column("verified_file_count", sa.Integer(), nullable=False, server_default="0"),
        sa.Column("verified_bytes", sa.BigInteger(), nullable=False, server_default="0"),
        sa.Column("error", sa.Text()),
    )
    op.create_index(
        "ix_verification_attempts_transfer_id", "verification_attempts", ["transfer_id"]
    )


def downgrade() -> None:
    op.drop_index("ix_verification_attempts_transfer_id", table_name="verification_attempts")
    op.drop_table("verification_attempts")
    op.drop_column("transfer_files", "verification_error")
    op.drop_column("transfer_files", "verified_at")
    op.drop_column("transfer_files", "verified_sha256")
    op.drop_column("transfer_files", "verified_size")
    op.drop_column("transfers", "verification_error")
    op.drop_column("transfers", "verification_attempt_count")
    op.drop_column("transfers", "verification_started_at")
