import asyncio

from datap_rs.datapress import DataPress, DataPressConfig, DatasetConfig


dataset_config = DatasetConfig(
    name="accidents",
    description="A dataset containing accident data",
    source="data/accidents.parquet",
    format="parquet",
    mode="auto",
)

config = DataPressConfig(
    backend="datafusion",
    listen="0.0.0.0",
    port=8000,
    workers=8,
)

datapress = DataPress(config, datasets=[dataset_config])


async def main() -> None:
    # Blocks until SIGINT (Ctrl-C).
    await datapress.run()


if __name__ == "__main__":
    asyncio.run(main())
