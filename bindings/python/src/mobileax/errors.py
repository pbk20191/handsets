"""Exception types raised by :class:`mobileax.Session`.

The CLI's structured exit codes (2 NOT_FOUND, 3 TIMEOUT, 4 AMBIGUOUS) get
broken-out subclasses since those three are the only ones scripts ever
branch on in practice. The longer tail (DAEMON_ERROR, BAD_ARG,
SECURE_WINDOW, ...) all collapse to exit 1 from the CLI but carry their
full ``ErrCode`` in JSON-mode output, so we surface them as distinct
exception types here too — anyone who *does* need to dispatch on them
can catch the specific subclass.
"""

from __future__ import annotations

from enum import Enum
from typing import Optional


class ErrCode(str, Enum):
    """Structured error code as emitted by ``mobax --json``."""

    NOT_FOUND = "NOT_FOUND"
    TIMEOUT = "TIMEOUT"
    AMBIGUOUS = "AMBIGUOUS"
    DAEMON_ERROR = "DAEMON_ERROR"
    DEVICE_GONE = "DEVICE_GONE"
    PRECONDITION = "PRECONDITION"
    BAD_ARG = "BAD_ARG"
    SECURE_WINDOW = "SECURE_WINDOW"
    UNKNOWN_CMD = "UNKNOWN_CMD"
    INTERNAL = "INTERNAL"


class MobileAXError(Exception):
    """Base class for all mobileAX failures."""

    code: ErrCode = ErrCode.INTERNAL

    def __init__(self, detail: str = "", *, verb: Optional[str] = None) -> None:
        self.detail = detail
        self.verb = verb
        msg = f"{self.code.value}: {detail}" if detail else self.code.value
        if verb:
            msg = f"{verb}: {msg}"
        super().__init__(msg)


class NotFound(MobileAXError):
    code = ErrCode.NOT_FOUND


class Timeout(MobileAXError):
    code = ErrCode.TIMEOUT


class Ambiguous(MobileAXError):
    code = ErrCode.AMBIGUOUS


class DaemonError(MobileAXError):
    code = ErrCode.DAEMON_ERROR


class DeviceGone(MobileAXError):
    code = ErrCode.DEVICE_GONE


class Precondition(MobileAXError):
    code = ErrCode.PRECONDITION


class BadArg(MobileAXError):
    code = ErrCode.BAD_ARG


class SecureWindow(MobileAXError):
    code = ErrCode.SECURE_WINDOW


class UnknownCmd(MobileAXError):
    code = ErrCode.UNKNOWN_CMD


_BY_CODE: dict[ErrCode, type[MobileAXError]] = {
    ErrCode.NOT_FOUND: NotFound,
    ErrCode.TIMEOUT: Timeout,
    ErrCode.AMBIGUOUS: Ambiguous,
    ErrCode.DAEMON_ERROR: DaemonError,
    ErrCode.DEVICE_GONE: DeviceGone,
    ErrCode.PRECONDITION: Precondition,
    ErrCode.BAD_ARG: BadArg,
    ErrCode.SECURE_WINDOW: SecureWindow,
    ErrCode.UNKNOWN_CMD: UnknownCmd,
    ErrCode.INTERNAL: MobileAXError,
}


def from_payload(verb: str, error: dict) -> MobileAXError:
    """Construct the right subclass from a JSON ``{"code": ..., "detail": ...}``."""
    raw = error.get("code", "INTERNAL")
    try:
        code = ErrCode(raw)
    except ValueError:
        code = ErrCode.INTERNAL
    cls = _BY_CODE.get(code, MobileAXError)
    return cls(error.get("detail", ""), verb=verb)
