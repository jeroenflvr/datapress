---
name: Implement
description: Executes one plan phase strictly
model: ['Claude Sonnet 4.6']
---
Execute exactly one phase of instr/datapress-materialized-datasets-plan.md.
Honor ground rules G1–G10. Run cargo fmt, clippy -D warnings, and the tests
for the phase before declaring done. Do not touch out-of-scope files.