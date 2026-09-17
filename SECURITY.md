# Security Policy

## Supported Versions

Photonic is in active early development. Security fixes are applied to the latest
`main` branch. There are no long-term support branches yet.

| Version | Supported |
| ------- | --------- |
| `main` (latest) | :white_check_mark: |
| older commits / tags | :x: |

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

Instead, report them privately using one of the following:

1. **GitHub Security Advisories** (preferred) — use the
   ["Report a vulnerability"](https://github.com/unn-corp/Photonic/security/advisories/new)
   button on the repository's Security tab. This keeps the report private until a
   fix is ready.
2. **Email** — send details to **security@theunnamed.dev** with "Photonic
   security" in the subject line.

Please include as much of the following as you can:

- A description of the vulnerability and its impact.
- Steps to reproduce, or a proof of concept.
- The affected component (GUI, MCP server, file parsing, Lua scripting, etc.)
  and commit/version.
- Any suggested remediation, if you have one.

### Scope notes

Photonic runs an **MCP server** (JSON-RPC over HTTP) and a **Lua scripting**
engine, and parses `.photon`/SVG files. Reports involving these surfaces are
especially valuable:

- The MCP server is intended for **local** use. Issues that let a remote or
  unauthorized party reach it, or that allow escaping its intended capabilities,
  are in scope.
- Lua scripts and opened documents are treated as **trusted input** today;
  sandbox-escape or arbitrary-code-execution reports via untrusted scripts or
  malicious files are welcome and in scope.

## Safe harbour

Photonic is maintained by The Unnamed Corp. Good faith research on this project
is authorised under our published
[Vulnerability Disclosure Policy](https://theunnamed.dev/security#vulnerability-disclosure)
on the same terms as the rest of our systems, and our
[Acceptable Use Policy](https://theunnamed.dev/legal/aup) treats research
conducted within that policy's scope and rules as permitted rather than as a
violation. Reporting to the addresses above keeps you inside that protection.

## Our Commitment

- We will acknowledge your report within **5 business days**.
- We will keep you informed of progress toward a fix.
- We will credit you in the release notes / advisory when the fix ships, unless
  you prefer to remain anonymous.

Thank you for helping keep Photonic and its users safe.
