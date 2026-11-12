# Desk-Remote — Core Engineering & Architecture Rules

This repository contains **Desk-Remote**, a low-overhead, self-hosted remote desktop solution designed specifically for Windows 10/11 environments.

All AI agents and human contributors working on this repository MUST strictly follow the architecture and engineering principles defined here. No exceptions are permitted without explicit owner approval.

---

## The Owner's Mandate

> *"Absolute folder discipline, central controls... Duplicate code must never be written; parts of the system that share functionality must consume it from a single source so maintenance cost remains low. Every file must speak the same language. For example, the logger must reside in one central location and determine the logging rules; rogue `console/println` statements must never be written across different files. The logger is just one example — systems like connection handling must also be centralized; connection code must never be duplicated in multiple places."*

---

## Non-Negotiable Core Rules

1. **Absolute Folder Discipline**: Code belongs exclusively to its designated crate or application directory. Ad-hoc helper modules or cross-boundary imports are strictly forbidden. See [Folder Layout](docs/architecture/folder-layout.md).
2. **Central Single-Point Systems**: Critical runtime systems (logging, config, paths, errors, transport/connections, protocol, crypto/identity, IPC, i18n, versioning, service lifecycle) are owned by dedicated crates. Directly implementing ad-hoc sockets, custom log formats, hardcoded paths, or duplicate serializers is strictly prohibited. See [Central Systems](docs/architecture/central-systems.md).
3. **Zero Duplicate Code**: Reusable logic belongs in `crates/`. Copy-pasting functions or structures between crates or apps will be rejected by automated duplication gates. See [Code Style](docs/architecture/code-style.md) and [Gates](docs/architecture/gates.md).
4. **Universal Code Style ("Every File Speaks the Same Language")**: Consistent naming, explicit error propagation (`Result<T, DeskError>`), module documentation headers, structured logging without raw `println!`/`eprintln!`/`dbg!`, and standardized linting. See [Code Style](docs/architecture/code-style.md).
5. **Automated Quality Gates**: CI and pre-commit checks automatically enforce `clippy` bans, formatting checks, copy-paste thresholds, and network boundary grep checks. See [Gates](docs/architecture/gates.md).
6. **Platform Scope**: Target platform is Windows 10/11 64-bit only. Architecture documents are platform-agnostic in principle, but system implementations leverage native Windows subsystem capabilities (SYSTEM service, session helper, DXGI duplication, Named Pipes).
7. **Public & MIT License Standard**: The repository is public. All code, comments, documentation, and commit messages MUST be written in English. User-facing text must be externalized via `desk-i18n` (English source + Turkish localization).
8. **Version Control**: Version is strictly pinned to `0.1.0` (read from the root [VERSION](VERSION) file). Do not bump versions or publish releases without explicit owner direction.

---

## Architecture Documentation Map

- [Folder Layout & Cargo Workspace Architecture](docs/architecture/folder-layout.md)
- [Central Systems & Single-Point Contracts](docs/architecture/central-systems.md)
- [Code Style & Engineering Conventions](docs/architecture/code-style.md)
- [Automated Enforcement Gates](docs/architecture/gates.md)
