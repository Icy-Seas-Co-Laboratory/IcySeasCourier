"""Retain encrypted invitation codes for administrator review.

Existing hash-only codes cannot be recovered; their ciphertext remains null.
"""

import sqlalchemy as sa

from alembic import op

revision = "0009"
down_revision = "0008"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.add_column("upload_invitations", sa.Column("code_ciphertext", sa.Text(), nullable=True))


def downgrade() -> None:
    op.drop_column("upload_invitations", "code_ciphertext")
