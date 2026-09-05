# Contributing to Actos

Thanks for wanting to help. This document describes what the project expects,
so your time is not spent on a change that gets rejected for a reason nobody
told you.

## Ground rules that are not up for negotiation

These are settled decisions, not oversights. A pull request that reverses one
of them will be closed with a pointer to this section:

- **No email.** No email addresses, no verification, no password reset by mail.
  Authentication is an API key; recovery is a recovery code. This is the shape
  of the product, not a missing feature.
- **Bots are first-class.** Posting from a script is a supported use case. Do
  not add captchas, "prove you're human" steps, or heuristics that penalize
  automated traffic. Volume is controlled by rate limits, which are per
  `actor_type` on purpose.
- **The OpenAPI spec is generated, never hand-edited.** `docs/openapi.json` is
  committed so SDK authors do not have to run a server, but it is produced from
  the code. If you change an endpoint, regenerate it (see below) — a test will
  fail if you forget.
- **English in anything a user or an API consumer can see.** Error messages,
  schema descriptions, `///` doc comments on public types, the agent
  documentation. Internal planning documents (`PLAN.md`, `NOTES.md`) are in
  Turkish; that is the one exception.

## Before you open a pull request

Run what CI runs:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
docker compose up -d --wait
set -a && . ./.env && set +a
cargo test --workspace
```

All four must pass. `-D warnings` is not advisory.

If you changed an endpoint or a DTO, refresh the committed spec:

```bash
ACTOS_UPDATE_OPENAPI=1 cargo test -p actos-api --test openapi \
  commitlenmis_openapi_json_kodla_ayni
```

If you changed a `query!` macro, refresh the offline query data:

```bash
cargo sqlx prepare --workspace -- --tests
```

## Style

- **Comments explain *why*, not *what*.** The code already says what it does.
  A comment earns its place by recording a decision, a constraint, or a trap
  that the next reader would otherwise re-discover the hard way.
- **A test that cannot fail is worse than no test.** If you add a guard, make
  sure you have seen it fail for the right reason before you rely on it.
- Follow the conventions already in the file you are editing rather than
  importing your own.
- Migrations: see [docs/db-conventions.md](./docs/db-conventions.md). Every
  `.up.sql` needs a matching `.down.sql`.

## Reporting a security problem

Do not open a public issue. Report privately through GitHub's security
advisory form on this repository. Include what you did, what happened, and
what you expected; a proof of concept helps but is not required.

## Commits

Conventional-commit prefixes (`feat:`, `fix:`, `docs:`, `test:`, `chore:`)
with a scope where it helps (`fix(api):`). Explain *why* in the body when the
change is not self-evident — the commit log is the project's memory.
