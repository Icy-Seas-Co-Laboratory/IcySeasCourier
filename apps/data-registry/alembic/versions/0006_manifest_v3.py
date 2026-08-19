"""Add v3 transport objects.

Revision ID: 0006
Revises: 0005
"""

from collections.abc import Sequence

import sqlalchemy as sa

from alembic import op

revision: str = "0006"
down_revision: str | None = "0005"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    op.drop_constraint("transfer_files_object_key_key", "transfer_files", type_="unique")
    op.create_table(
        "transfer_objects",
        sa.Column("id", sa.Uuid(), primary_key=True),
        sa.Column(
            "transfer_id",
            sa.Uuid(),
            sa.ForeignKey("transfers.id", ondelete="CASCADE"),
            nullable=False,
        ),
        sa.Column("kind", sa.String(20), nullable=False),
        sa.Column("compression", sa.String(20), nullable=False),
        sa.Column("encoding_version", sa.Integer(), nullable=False),
        sa.Column("original_bytes", sa.BigInteger(), nullable=False),
        sa.Column("object_key", sa.Text(), nullable=False, unique=True),
        sa.Column("multipart_upload_id", sa.Text()),
        sa.Column("status", sa.String(30), nullable=False, server_default="pending"),
        sa.Column("created_at", sa.DateTime(timezone=True), server_default=sa.func.now()),
        sa.Column("completed_at", sa.DateTime(timezone=True)),
    )
    op.add_column("transfer_files", sa.Column("transport_object_id", sa.Uuid()))
    op.add_column("transfer_files", sa.Column("member_index", sa.Integer()))
    op.create_index(
        "ix_transfer_files_transport_object_id", "transfer_files", ["transport_object_id"]
    )
    op.create_foreign_key(
        "fk_transfer_files_transport_object_id",
        "transfer_files",
        "transfer_objects",
        ["transport_object_id"],
        ["id"],
        ondelete="CASCADE",
    )
    op.create_table(
        "transport_parts",
        sa.Column("id", sa.Uuid(), primary_key=True),
        sa.Column(
            "transport_object_id",
            sa.Uuid(),
            sa.ForeignKey("transfer_objects.id", ondelete="CASCADE"),
            nullable=False,
        ),
        sa.Column("part_number", sa.Integer(), nullable=False),
        sa.Column("etag", sa.Text(), nullable=False),
        sa.Column("size", sa.BigInteger()),
        sa.Column("recorded_at", sa.DateTime(timezone=True), server_default=sa.func.now()),
        sa.UniqueConstraint("transport_object_id", "part_number"),
    )
    op.execute(
        "UPDATE transfers SET status='failed', "
        "verification_error='legacy manifest version unsupported after v3 migration' "
        "WHERE manifest_version <> 3 "
        "AND status NOT IN ('complete', 'failed', 'cancelled')"
    )


def downgrade() -> None:
    op.drop_table("transport_parts")
    op.drop_constraint(
        "fk_transfer_files_transport_object_id", "transfer_files", type_="foreignkey"
    )
    op.drop_index("ix_transfer_files_transport_object_id", table_name="transfer_files")
    op.drop_column("transfer_files", "member_index")
    op.drop_column("transfer_files", "transport_object_id")
    op.drop_table("transfer_objects")
    op.create_unique_constraint("transfer_files_object_key_key", "transfer_files", ["object_key"])
