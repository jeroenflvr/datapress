from datapress import DataPress, DataPressConfig, DataSetConfig


dataset_config = DataSetConfig(
    name="accidents",
    description="A dataset containing accident data",
    source="data/accidents.parquet",
    format="parquet",
    mode="auto",
)

config = DataPressConfig(
    backend="duckdb",
    listen="0.0.0.0",
    port="8000",
    workers=8,
)