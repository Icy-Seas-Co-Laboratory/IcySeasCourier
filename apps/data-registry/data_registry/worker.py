import logging
import signal
import time

from .config import get_settings
from .db import SessionLocal
from .logging_config import configure_logging
from .storage import get_object_storage
from .verification import claim_transfer, verify_claim

running = True


def stop(*_args) -> None:
    global running
    running = False


def main() -> None:
    configure_logging()
    logger = logging.getLogger("data_registry.verification")
    settings = get_settings()
    storage = get_object_storage()
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    logger.info("verification_worker_started")
    while running:
        try:
            with SessionLocal() as database:
                claim = claim_transfer(database, settings)
        except Exception:
            logger.exception("verification_claim_failed")
            time.sleep(settings.verification_poll_seconds)
            continue
        if claim is None:
            time.sleep(settings.verification_poll_seconds)
            continue
        logger.info("verification_started", extra={"transfer_id": str(claim.transfer_id)})
        try:
            with SessionLocal() as database:
                verify_claim(database, storage, settings, claim)
        except Exception:
            logger.exception("verification_execution_failed")
            time.sleep(settings.verification_poll_seconds)
            continue
        logger.info("verification_finished", extra={"transfer_id": str(claim.transfer_id)})
    logger.info("verification_worker_stopped")


if __name__ == "__main__":
    main()
