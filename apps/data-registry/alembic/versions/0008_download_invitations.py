"""Add project-scoped download invitations.

Revision ID: 0008
Revises: 0007
"""

import sqlalchemy as sa

from alembic import op

revision = "0008"
down_revision = "0007"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.add_column(
        "upload_invitations",
        sa.Column("purpose", sa.String(20), nullable=False, server_default="upload"),
    )


def downgrade() -> None:
    op.drop_column("upload_invitations", "purpose")
