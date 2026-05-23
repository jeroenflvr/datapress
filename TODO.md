# TODO for python wrapper

- change to workspace
- move to core (lib vs bin)
- source is parquet or deltalake, to always be read into memory for both duckdb and arrow
- pyo3/maturin
- pydantic_settings
- load data from s3 into memory
- add endpoint to reload data (switch pointer): requires the dataset to fit twice in memory
- add versioning on the api /api/v1 and respect hierarchy in the project structure: handlers/v1
- allow api from behind reverse proxy route, ie /fast-api => /fast-api/api/datasets/{name}/query


Python
- pydantic_settings
- config
  - number of workers (all cores if None)
  - dataset location
  - duckdb vs datafusion
  - index mode auto or list (+ define list)
  - port
  - listen address (127.0.0.1 default)
  - 