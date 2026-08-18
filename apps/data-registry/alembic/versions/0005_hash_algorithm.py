"""Record the configured logical-file hash algorithm.

Revision ID: 0005
Revises: 0004
"""

import sqlalchemy as sa

from alembic import op

revision = "0005"
down_revision = "0004"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.add_column(
        "transfers",
        sa.Column("hash_algorithm", sa.String(length=20), nullable=False, server_default="sha256"),
    )
    op.add_column(
        "transfer_files",
        sa.Column("hash_algorithm", sa.String(length=20), nullable=False, server_default="sha256"),
    )


def downgrade() -> None:
    op.drop_column("transfer_files", "hash_algorithm")
    op.drop_column("transfers", "hash_algorithm")
