# Packaging Medis

## macOS

Requires only the Rust toolchain and built-in macOS tools (`plutil`, `codesign`, `hdiutil`, `python3`).

```sh
./scripts/package-macos.sh              # build, bundle, sign, create dmg
./scripts/package-macos.sh --skip-build # reuse existing release binary
./scripts/package-macos.sh --no-dmg     # only produce dist/Medis.app
```

Outputs (under `medis-rs/dist/`):

- `Medis.app`
- `Medis-<version>-macos-<arch>.dmg` (`arm64` or `x86_64`)

Build for the other architecture with `TARGET` (install it first with `rustup target add`):

```sh
TARGET=x86_64-apple-darwin ./scripts/package-macos.sh   # Intel build on an Apple Silicon Mac
```

The version comes from `cargo metadata` (workspace version). The script is
safe to rerun; it replaces `dist/Medis.app` each time.

### Environment variables

| Variable          | Purpose |
|-------------------|---------|
| `TARGET`          | Rust target triple: `aarch64-apple-darwin` or `x86_64-apple-darwin`. Unset = the host's architecture. |
| `CARGO_TARGET_DIR`| Custom cargo target dir; the binary is read from `$CARGO_TARGET_DIR/[$TARGET/]release/medis`. |
| `SIGN_IDENTITY`   | Codesign identity (e.g. `Developer ID Application: Name (TEAMID)`). Enables hardened runtime + timestamp. Unset = ad-hoc signing (`-s -`). |
| `NOTARY_PROFILE`  | `notarytool` keychain profile (`xcrun notarytool store-credentials`). Requires `SIGN_IDENTITY`. Submits the app, waits, and staples. Unset = skipped. |
| `BUNDLE_ID`       | Bundle identifier, default `com.medis.app`. |

### Gatekeeper and ad-hoc builds

Ad-hoc signed builds are not notarized, so Gatekeeper blocks them when
downloaded. Either:

- right-click `Medis.app` and choose **Open** (then confirm), or
- strip the quarantine attribute:

  ```sh
  xattr -dr com.apple.quarantine /Applications/Medis.app
  ```

Locally built apps are not quarantined and open normally.

## Linux and Windows

GitHub Actions (`.github/workflows/build.yml`) builds every platform when a `v*` tag is pushed,
and publishes a GitHub release with all of them:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

To build without releasing, use **Actions → Build → Run workflow**; the packages are under the
run's Artifacts.
