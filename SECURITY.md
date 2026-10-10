# Rho Core security boundary

`main` currently contains a rebuild design, not an executable runtime. The former
implementation and its security assumptions are retained on
`codex/legacy-core-before-rebuild`. This document states requirements for new code;
it does not certify an implemented sandbox or security feature.

The first selected deployment is a trusted, single-user local project. Tool and
target validation protects the actual boundary it can enforce. File containment
does not isolate arbitrary executed code, which may use the user's OS privileges
and network. Model output, tool descriptions and skill files cannot establish
authority or override native execution conditions.

New network transports must define authentication, exposure and resource limits
before use. The trust of a local stdio connection does not transfer to HTTP.
Remote sharing, role policies and OS sandboxing require separately selected
requirements and verification. Already authorized local work does not receive an
additional Rho approval step.

For suspected vulnerabilities, use
[private vulnerability reporting](https://github.com/YuLab-SMU/Rho/security/advisories/new).
Include the affected source or artifact, platform, impact and minimal reproduction.
Keep credentials, private project content and unredacted diagnostics out of public
reports. Historical development artifacts should not be assumed to receive fixes.
Data-handling requirements are described in [PRIVACY](PRIVACY.md).
