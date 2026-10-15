# Forza branch dependency review — 2026-10-15

This review covers the locked Linux service, CLI/login, and overlay dependency
graph. It is not a guarantee that the complete Proton distribution or host has
no vulnerabilities. Host OpenSSL/WebKit and Flatpak runtime updates remain relevant.

The prior `7b236772` graph contained advisories in anyhow, cryptoki, openssl,
quick-xml, rustls, rustls-webpki, h2, and glib. This branch updates the compatible
packages, removes reqwest 0.11 and its old TLS graph from Xal/OAuth, and backports
the GLib iterator correction. `Cargo.lock` records the resolved versions.
The vendored sources and changes are documented in [vendor/README.md](vendor/README.md).

The fresh OSV registry-package query still reports:

| Dependency | Advisory | Disposition |
| --- | --- | --- |
| rsa 0.9.10 | [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html) / CVE-2023-49092 | No upstream patched version. Xodus SOAP RSA-SHA256 signing now uses OpenSSL through Bergshamra's signer hook. The RustCrypto crate remains for key parsing and public verification. The reviewed Xodus SOAP decrypt path uses AES; it does not call RSA decryption or generic Bergshamra XML decryption. This is a scoped application mitigation, not a patched-crate claim or a blanket exemption for future code. |
| proc-macro-error 1.0.4 | [RUSTSEC-2024-0370](https://rustsec.org/advisories/RUSTSEC-2024-0370.html) | Unmaintained compile-time procedural-macro dependency in the GTK3 login stack. No runtime vulnerability is asserted by this advisory; replacing the GTK3/WebKit stack is separate work. |

OpenSSL signing uses explicit SHA-256 and PKCS#1 v1.5 padding, imports a temporary
zeroizing PKCS#8 SecretDocument, and reports fixed error messages without key data.
Local tests verify signatures with the existing RustCrypto public verifier and
verify a complete generated SOAP request, including rejection after scope tampering.
The existing SOAP verification policy is unchanged.

OAuth transport tests verify redirect refusal and the response-size bound. Live
Microsoft tests remain ignored by default; the previously automatic device-token
test is now explicit and no longer prints a device token. Unit tests do not prove
that a real Microsoft login, Forza entitlement, multiplayer invite, or physical
Steam Deck controller works. Those results must be reported separately.

CI uses the pinned owned Linux builder image and tests the shipped packages and
optimized GLib iterator backport. No GitHub token is persisted in the checkout.
