# Security Policy

## Supported versions

This project is pre-1.0 and under active development. Security fixes are
applied to the latest released version on the `main` branch. Please make sure
you're on the most recent release before reporting.

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues,
pull requests, or discussions.**

Instead, use GitHub's private vulnerability reporting:

1. Go to the [Security tab](https://github.com/jeroenflvr/fast-api/security) of
   the repository.
2. Click **"Report a vulnerability"** to open a private advisory.

Please include:

- A description of the issue and the affected component (e.g. `datapress-core`,
  a specific backend, the Python wheel, or the auth/object-store paths).
- Steps to reproduce or a proof of concept.
- The version / commit you tested against.
- Any suggested remediation, if you have one.

## What to expect

- We'll acknowledge your report as soon as we reasonably can.
- We'll investigate, keep you updated on progress, and coordinate a fix and
  disclosure timeline with you.
- We'll credit you in the release notes unless you prefer to remain anonymous.

## Dependency advisories

Dependencies are scanned for known RUSTSEC advisories on every CI run via
[`cargo audit`](.github/workflows/ci.yml), and dependency updates are proposed
automatically through [Dependabot](.github/dependabot.yml).
