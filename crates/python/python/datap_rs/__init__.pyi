"""Type stubs for the top-level ``datap_rs`` package."""

from . import datapress as datapress
from .datapress import (
    AuthConfig as AuthConfig,
    DataPress as DataPress,
    DataPressConfig as DataPressConfig,
    DatasetConfig as DatasetConfig,
    S3Config as S3Config,
)
from .client import (
    DataPressClient as DataPressClient,
    DataPressHTTPError as DataPressHTTPError,
)

__all__ = [
    "datapress",
    "AuthConfig",
    "DataPress",
    "DataPressConfig",
    "DatasetConfig",
    "S3Config",
    "DataPressClient",
    "DataPressHTTPError",
]
