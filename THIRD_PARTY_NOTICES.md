# Third-Party Notices

Meatus is distributed under the Apache License 2.0 (see `LICENSE`).

## Web panel

The Web panel consumes the following packages from the open-source Sailonix Vue 3 component library:

| Package | Version | License | Upstream |
| --- | --- | --- | --- |
| `@sailonix/ui` | `0.1.0` | MIT | <https://gitee.com/binsail/sailon> |
| `@sailonix/icon` | `0.1.0` | MIT | <https://gitee.com/binsail/sailon> |

They are installed from the npm registry and pinned by `web/package-lock.json`. The published tarballs include the upstream MIT `LICENSE` file; that license and copyright notice apply to these packages.

`@sailonix/ui` also depends on `qrcode-generator` (MIT), which is resolved by npm and recorded in `web/package-lock.json`.

The Web panel also bundles Vue 3.5.x and Vue Router 4.6.x (both MIT) plus their transitive runtime dependencies. See `docs/THIRD_PARTY_LICENSES.md` for the generated package-by-package license inventory. The bundled upstream license texts are in `docs/THIRD_PARTY_LICENSE_TEXTS.txt`.

## Other dependencies

Other Rust and Web runtime / build dependencies are declared in `Cargo.toml`, `web/package.json` and their lockfiles. Their own licenses apply. If a dependency is redistributed under terms other than Apache-2.0, those terms apply to that dependency and must be listed here before publishing a release.

## Release checklist

- Include this file, `LICENSE`, `docs/THIRD_PARTY_LICENSES.md` and `docs/THIRD_PARTY_LICENSE_TEXTS.txt` in release archives and Docker images.
- Ship the upstream license text / copyright notice for every MIT, BSD, ISC, MPL-2.0 and other non-Apache dependency included in a binary artifact.
- Keep `web/package-lock.json` pinned; do not replace published packages with an untracked local path.
- Re-check the upstream license for every `@sailonix/*` upgrade.
