"""datap-rs — fast multi-backend dataset HTTP server.

The compiled extension is exposed as the :mod:`datap_rs.datapress`
submodule. Typical usage::

    from datap_rs import datapress

    cfg = datapress.DataPressConfig(...)
    dp  = datapress.DataPress(cfg)

The public classes are also re-exported at the top level for
convenience::

    from datap_rs import DataPress, DataPressConfig, DatasetConfig, S3Config
"""

from . import datapress
from .datapress import (
    AuthConfig,
    DataPress,
    DataPressConfig,
    DatasetConfig,
    HMACKeyPair,
    S3Config,
)
from .client import DataPressClient, DataPressHTTPError

__all__ = [
    "datapress",
    "AuthConfig",
    "DataPress",
    "DataPressConfig",
    "DatasetConfig",
    "HMACKeyPair",
    "S3Config",
    "DataPressClient",
    "DataPressHTTPError",
]
