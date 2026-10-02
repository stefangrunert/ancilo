# Security

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub's **"Report a vulnerability"** (Security → Advisories) in this repository – not in public issues. You will get an answer within a few days. Please include the version (`ancilo --version`), your system and steps to reproduce.

## What Ancilo protects

- The daemon accepts connections only on the loopback interface, requires a bearer token for everything except the health check, and rejects foreign `Host` and `Origin` headers (protection against DNS rebinding and malicious web pages).
- Terminal connections use one-time tickets instead of the token.
- Operations with consequences (removing models, applying changes, deleting sessions, …) need an explicit confirmation.
- Delegated tasks that may run commands run in a sandbox (Seatbelt on macOS, Bubblewrap on Linux): writing only inside the project and temporary directories, no network.
- Coding sessions work in a separate work area; the project changes only when you apply the changes.
- Downloads (models, llama.cpp) are verified by size and SHA-256.
- Cloud API keys live in the system keychain.

## Supported versions

Security fixes go into the latest release.
