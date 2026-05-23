# TODO for python wrapper

- pydantic_settings
- add endpoint to reload data (load data into new area, switch pointer, delete old area): requires the dataset to fit twice in memory
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
  - listen address (127.0.0.1 default, don't expose by default)
  - 