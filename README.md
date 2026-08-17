# CHROMATRON

A Rust engine for large-scale 3D simulations, with games built on top. Desktop only.

**Start here: [`docs/00-INDEX.md`](docs/00-INDEX.md)** — it is a routing table, not a table of contents. Read it, then read only the files it points you to for your task.

## Reading order for a new agent or contributor

1. `docs/00-INDEX.md` — routing table and status board
2. `docs/03-conventions.md` — units, tick semantics, determinism rules, error policy. **Required before writing any code.**
3. `docs/04-glossary.md` — terms defined once
4. The five ADRs named at the top of the index
5. Your task's spec and milestone, per the routing table

## Layout

| Path | Contents |
|---|---|
| `docs/00-INDEX.md` | Routing table, spec status board, milestone order |
| `docs/01-scope.md` | What this is, what it is not, design targets, reference points |
| `docs/02-architecture.md` | Module composition, the three-way split, crate graph, tick lifecycle |
| `docs/03-conventions.md` | Units, coordinates, time, determinism, errors, naming, testing |
| `docs/04-glossary.md` | Canonical definitions |
| `docs/specs/` | S01–S20 — the source of truth for *what* |
| `docs/milestones/` | M0–M10 — the source of truth for *when and in what order* |
| `docs/adr/` | Decision records — the source of truth for *why*. Append-only. |
| `docs/bench/` | Benchmark baselines and memory budget — both are CI gates |

## Rules for maintaining these docs

- Never restate a fact across specs, milestones, and ADRs. Link instead.
- Update `status` in a spec's front matter when you work on it; append to `open_questions` when you discover something the spec did not anticipate.
- Do not edit ADRs. Write a new one that supersedes or clarifies.
- A milestone is not complete until its section in `docs/bench/baselines.md` passes in CI.
