# Rho core

Read README.md and inspect git status before editing. This repository owns the
generic Host, journal, protocol, SDK and public transport edges. Do not import
scientific plugin source, application UI or R examples. Public generated SDKs
are maintained here and exported as versioned dependency snapshots.

Use focused behavior tests, not the retired whole-product suite. Choose checks
for the changed effect or boundary; never run Cargo commands concurrently in a
shared target directory. Core builds must work without either sibling checkout.
Preserve caller/project containment, original operations, idempotency and uncertain
outcomes. Do not start or restart a user's Host/R session for verification.

The current simplification is a breaking upgrade, verified with fresh projects
and storage. Old projects, directories and history are not migration or recovery
requirements. Remove retired contracts and storage code outright; do not add
compatibility wrappers, data exports or restoration paths for them. Preserve the
execution guarantees of Operations accepted under the new contract.

Commit coherent changes. Refresh consumer dependency snapshots explicitly after
committing core changes; do not edit their copies in other repositories. Local
development and application assembly are coordinated from the Rho repository.
