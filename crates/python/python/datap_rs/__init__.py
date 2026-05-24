"""datap-rs — fast multi-backend dataset HTTP server.

The compiled extension is exposed as the :mod:`datap_rs.datapress`
submodule. Typical usage::

    from datap_rs import datapress

    cfg = datapress.DataPressConfig(...)
    dp  = datapress.DataPress(cfg)

The four public classes are also re-exported at the top level for
convenience::

    from datap_rs import DataPress, DataPressConfig, DatasetConfig, S3Config
"""

from . import datapress
from .datapress import DataPress, DataPressConfig, DatasetConfig, S3Config

__all__ = [
    "datapress",
    "DataPress",
    "DataPressConfig",
    "DatasetConfig",
    "S3Config",
]
