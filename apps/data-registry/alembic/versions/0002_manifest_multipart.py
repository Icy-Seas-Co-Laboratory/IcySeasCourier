"""Immutable manifests and multipart upload records.

Revision ID: 0002
Revises: 0001
"""

import sqlalchemy as sa

from alembic import op

revision = "0002"
down_revision = "0001"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.add_column("transfers", sa.Column("manifest", sa.JSON()))
    op.add_column("transfers", sa.Column("manifest_sha256", sa.String(64)))
    op.add_column("transfers", sa.Column("manifest_submitted_at", sa.DateTime(timezone=True)))
    op.create_table(
        "transfer_files",
        sa.Column("id", sa.Uuid(), primary_key=True),
        sa.Column(
            "transfer_id",
            sa.Uuid(),
            sa.ForeignKey("transfers.id", ondelete="CASCADE"),
            nullable=False,
        ),
        sa.Column("relative_path", sa.Text(), nullable=False),
        sa.Column("original_size", sa.BigInteger(), nullable=False),
        sa.Column("original_sha256", sa.String(64), nullable=False),
        sa.Column("modified_at", sa.DateTime(timezone=True), nullable=False),
        sa.Column("compression", sa.String(20), nullable=False),
        sa.Column("transport_encoding_version", sa.Integer(), nullable=False),
        sa.Column("object_key", sa.Text(), nullable=False, unique=True),
        sa.Column("multipart_upload_id", sa.Text()),
        sa.Column("status", sa.String(30), nullable=False),
        sa.Column("created_at", sa.DateTime(timezone=True), server_default=sa.func.now()),
        sa.Column("completed_at", sa.DateTime(timezone=True)),
        sa.UniqueConstraint("transfer_id", "relative_path"),
    )
    op.create_table(
        "transfer_parts",
        sa.Column("id", sa.Uuid(), primary_key=True),
        sa.Column(
            "file_id",
            sa.Uuid(),
            sa.ForeignKey("transfer_files.id", ondelete="CASCADE"),
            nullable=False,
        ),
        sa.Column("part_number", sa.Integer(), nullable=False),
        sa.Column("etag", sa.Text(), nullable=False),
        sa.Column("size", sa.BigInteger()),
        sa.Column("recorded_at", sa.DateTime(timezone=True), server_default=sa.func.now()),
        sa.UniqueConstraint("file_id", "part_number"),
    )


def downgrade() -> None:
    op.drop_table("transfer_parts")
    op.drop_table("transfer_files")
    op.drop_column("transfers", "manifest_submitted_at")
    op.drop_column("transfers", "manifest_sha256")
    op.drop_column("transfers", "manifest")
