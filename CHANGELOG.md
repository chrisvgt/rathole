# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [v0.5.1] - 2025-10-28

### Changed
- **Breaking**: Renamed feature flag `tls` to `native-tls` for better clarity
- **Breaking**: Renamed feature flag `websocket` to `websocket-native-tls` to distinguish TLS implementations
- Refactored default features to use `native-tls` instead of `tls`
- Improved Cargo.toml formatting and organization

### Added
- Added `rustls` feature flag for rustls-based TLS support as an alternative to native-tls
- Added `websocket-rustls` feature flag for WebSocket with rustls support
- Added rustls-related dependencies:
  - `tokio-rustls` (0.25)
  - `rustls-pemfile` (2.0)
  - `rustls-native-certs` (0.7)
  - `p12` (0.6.3)
- Added platform-specific OpenSSL dependency for musl targets with vendored feature
- Added platform-specific build-dependencies configuration:
  - Windows: `vergen` without git features
  - Non-Windows: `vergen` with git features

### Fixed
- Improved cross-platform compatibility with platform-specific dependency configurations

## [v0.5.0] - 2023-10-01

Initial tracked release.

---

**Note**: This changelog summarizes the differences between v0.5.0 and v0.5.1 based on the commit history and code analysis.

[v0.5.1]: https://github.com/chrisvgt/rathole/compare/v0.5.0...v0.5.1
[v0.5.0]: https://github.com/chrisvgt/rathole/releases/tag/v0.5.0
