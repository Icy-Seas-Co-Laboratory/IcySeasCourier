"""Initial Registry control plane.

Revision ID: 0001
Revises: none
"""

import sqlalchemy as sa

from alembic import op

revision = "0001"
down_revision = None
branch_labels = None
depends_on = None


def timestamps() -> list[sa.Column]:
    return [
        sa.Column("created_at", sa.DateTime(timezone=True), server_default=sa.func.now()),
        sa.Column("updated_at", sa.DateTime(timezone=True), server_default=sa.func.now()),
    ]


def upgrade() -> None:
    op.create_table(
        "users",
        sa.Column("id", sa.Uuid(), primary_key=True),
        sa.Column("email", sa.String(320), nullable=False, unique=True),
        sa.Column("display_name", sa.String(200), nullable=False),
        sa.Column("active", sa.Boolean(), nullable=False),
        sa.Column("role", sa.String(30), nullable=False),
        *timestamps(),
    )
    op.create_table(
        "projects",
        sa.Column("id", sa.Uuid(), primary_key=True),
        sa.Column("project_code", sa.String(6), nullable=False, unique=True),
        sa.Column("name", sa.String(300), nullable=False),
        sa.Column("description", sa.Text()),
        sa.Column("status", sa.String(30), nullable=False),
        *timestamps(),
    )
    op.create_index("ix_projects_project_code", "projects", ["project_code"], unique=True)
    op.create_table(
        "upload_invitations",
        sa.Column("id", sa.Uuid(), primary_key=True),
        sa.Column("token_hash", sa.String(64), nullable=False, unique=True),
        sa.Column("expires_at", sa.DateTime(timezone=True), nullable=False),
        sa.Column("maximum_transfer_bytes", sa.BigInteger()),
        sa.Column("maximum_uses", sa.Integer()),
        sa.Column("use_count", sa.Integer(), nullable=False),
        sa.Column("created_by", sa.String(320), nullable=False),
        sa.Column("created_at", sa.DateTime(timezone=True), server_default=sa.func.now()),
        sa.Column("revoked_at", sa.DateTime(timezone=True)),
    )
    op.create_index(
        "ix_upload_invitations_token_hash", "upload_invitations", ["token_hash"], unique=True
    )
    op.create_table(
        "invitation_projects",
        sa.Column(
            "invitation_id",
            sa.Uuid(),
            sa.ForeignKey("upload_invitations.id", ondelete="CASCADE"),
            primary_key=True,
        ),
        sa.Column(
            "project_id",
            sa.Uuid(),
            sa.ForeignKey("projects.id", ondelete="CASCADE"),
            primary_key=True,
        ),
    )
    op.create_table(
        "courier_sessions",
        sa.Column("id", sa.Uuid(), primary_key=True),
        sa.Column("token_hash", sa.String(64), nullable=False, unique=True),
        sa.Column("invitation_id", sa.Uuid(), sa.ForeignKey("upload_invitations.id")),
        sa.Column("client_identifier", sa.String(200), nullable=False),
        sa.Column("courier_version", sa.String(40), nullable=False),
        sa.Column("created_at", sa.DateTime(timezone=True), server_default=sa.func.now()),
        sa.Column("expires_at", sa.DateTime(timezone=True), nullable=False),
    )
    op.create_index(
        "ix_courier_sessions_token_hash", "courier_sessions", ["token_hash"], unique=True
    )
    op.create_table(
        "transfers",
        sa.Column("id", sa.Uuid(), primary_key=True),
        sa.Column("public_id", sa.String(40), nullable=False, unique=True),
        sa.Column("project_id", sa.Uuid(), sa.ForeignKey("projects.id")),
        sa.Column("invitation_id", sa.Uuid(), sa.ForeignKey("upload_invitations.id")),
        sa.Column("client_session_id", sa.Uuid(), sa.ForeignKey("courier_sessions.id")),
        sa.Column("idempotency_key", sa.String(100), nullable=False),
        sa.Column("courier_version", sa.String(40), nullable=False),
        sa.Column("manifest_version", sa.Integer(), nullable=False),
        sa.Column("source_name", sa.String(512), nullable=False),
        sa.Column("file_count", sa.Integer(), nullable=False),
        sa.Column("original_bytes", sa.BigInteger(), nullable=False),
        sa.Column("transport_bytes", sa.BigInteger()),
        sa.Column("status", sa.String(30), nullable=False),
        sa.Column("created_at", sa.DateTime(timezone=True), server_default=sa.func.now()),
        sa.Column("started_at", sa.DateTime(timezone=True)),
        sa.Column("completed_at", sa.DateTime(timezone=True)),
        sa.Column("verified_at", sa.DateTime(timezone=True)),
        sa.UniqueConstraint("client_session_id", "idempotency_key"),
    )
    op.create_index("ix_transfers_public_id", "transfers", ["public_id"], unique=True)
    op.create_table(
        "audit_events",
        sa.Column("id", sa.Uuid(), primary_key=True),
        sa.Column("timestamp", sa.DateTime(timezone=True), server_default=sa.func.now()),
        sa.Column("actor", sa.String(320), nullable=False),
        sa.Column("action", sa.String(100), nullable=False),
        sa.Column("object_type", sa.String(80), nullable=False),
        sa.Column("object_id", sa.String(80), nullable=False),
        sa.Column("event_metadata", sa.JSON(), nullable=False),
    )


def downgrade() -> None:
    for table in (
        "audit_events",
        "transfers",
        "courier_sessions",
        "invitation_projects",
        "upload_invitations",
        "projects",
        "users",
    ):
        op.drop_table(table)
