<p align="center">
  <img src="docs/logo.svg" width="220" alt="Cobalt">
</p>

<p align="center"><strong>Apps and an SDK for Kobo e-readers.</strong></p>

Cobalt is an open-source application platform for Kobo. It provides a launcher,
an App Store, a Rust SDK, a runtime with capability isolation, and a Clara BW
simulator.

After one USB installation, users can install, update, and remove signed apps
over Wi-Fi. App releases are independent from Cobalt platform releases, so a
new app can appear in Store without reinstalling or updating Cobalt.

> [!IMPORTANT]
> The **Kobo Clara BW N365 (device code 391)**, **Kobo Elipsa 2E N605 (device
> code 389)**, and **Kobo Clara HD N249 (device code 376)** are fully
> hardware-tested on the exact firmware and kernel versions in the support
> matrix. See the
> [device support matrix](docs/DEVICES.md#device-support-matrix) before
> installing.
> It is an independent project and is not affiliated with Rakuten Kobo.

> [!TIP]
> **Own an unsupported Kobo? Help test its port.** No coding is required.
> Read the thread and comment with your exact model, firmware, and whether you
> can run attended tests:
> [Libra Colour](https://github.com/BandarLabs/Cobalt/issues/28),
> [Libra 2](https://github.com/BandarLabs/Cobalt/issues/29),
> [Clara Colour](https://github.com/BandarLabs/Cobalt/issues/30),
> or [Aura](https://github.com/BandarLabs/Cobalt/issues/32).
> Start with read-only checks; run panel tests only against the commit named by
> a maintainer. See
> [Contributing](CONTRIBUTING.md#device-testing).

## Features

- Signed Wi-Fi app installation, updates, and removal
- Separate Settings-based updates for the Cobalt platform
- Apps run as separate unprivileged processes
- Per-app capability checks for network, storage, audio, frontlight, and other
  device services
- Declarative e-ink UI toolkit and browser simulator
- Profile-driven full and partial refresh planning for supported panels
- Static ARMv7 binaries with no device-side package manager
- Recovery-safe app and catalog transactions

## How it differs

[NickelMenu](https://pgaskin.net/NickelMenu/) adds actions to Kobo's stock
menu. [KOReader](https://koreader.rocks/) and
[Plato](https://github.com/baskerville/plato) are reading apps. Cobalt is a
platform for building and installing apps.

Cobalt handles the common parts: screens, app lifecycle, drawing to the e-ink
display, partial refreshes, touch input, device access, process isolation,
testing, and signed installs. App authors can focus on their app instead of
building those parts again.
See the [FAQ](https://bandarlabs.github.io/Cobalt/faq.html) for a fuller
comparison.

## Apps

Every screenshot below is a real capture from a Kobo Clara BW. Store manages
the installable applications; Settings and Terminal remain protected system
utilities.

<table>
<tr>
<td width="33%" valign="top"><a href="examples/launcher/README.md"><img width="230" src="examples/launcher/screenshots/home.png" alt="Cobalt launcher showing a grid of applications"></a><br><b><a href="examples/launcher/README.md">Launcher</a></b><br>Opens installed apps and always keeps a route back to the Kobo reader.</td>
<td width="33%" valign="top"><a href="docs/APP_STORE.md"><img width="230" src="examples/store/screenshots/catalog.png" alt="Cobalt App Store listing installed and available applications"></a><br><b><a href="docs/APP_STORE.md">App Store</a></b><br>Browses signed apps and installs, updates, removes, and reinstalls them over Wi-Fi.</td>
<td width="33%" valign="top"><a href="examples/terminal/README.md"><img width="230" src="examples/terminal/screenshots/shell.png" alt="A shell and touch keyboard on the Kobo display"></a><br><b><a href="examples/terminal/README.md">Terminal</a></b><br>A panel-native shell with keys that send input immediately.</td>
</tr>
<tr>
<td valign="top"><a href="examples/settings/README.md"><img width="230" src="examples/settings/screenshots/battery.png" alt="Battery status and hardware facts"></a><br><b><a href="examples/settings/README.md">Settings</a></b><br>Manages connectivity and hardware, and keeps platform updates separate from Store.</td>
<td valign="top"><b><a href="apps/pret-numerique/">Prêt numérique</a></b><br>Borrows and returns Montréal and BAnQ library loans, and follows each request where it was started.</td>
</tr>
</table>

## Install

Install Rust, add the ARM target and connect a charged, fully supported reader
over USB:

```sh
git clone https://github.com/BandarLabs/Cobalt.git
cd Cobalt
rustup target add armv7-unknown-linux-musleabihf
cargo run -p kobo-cli -- setup
```

Restart the reader and open **Cobalt** from Kobo's menu. Future applications
are installed from **Store** over Wi-Fi. Full Cobalt updates remain under
**Settings**.

If you already use NickelMenu, Cobalt is added to it; existing entries are
left alone.

See [docs/INSTALL.md](docs/INSTALL.md) for the complete walkthrough and
recovery steps.

## App Store

Store reads a signed catalog from the fixed `app-catalog` GitHub release. Each
package contains one ARM executable and a signed canonical manifest. The
runtime verifies the catalog, package, installed manifest, and binary before
launch.

Store is the only app-management surface. The applications bundled with the
first `0.2.0` platform install appear as installed, can be removed and
reinstalled in the same session, and can be updated in place without creating
a second launcher entry. Platform utilities such as Settings and Terminal are
shown as installed system apps and cannot be removed.

Apps are published automatically when an app PR is merged into `main`.
Publishing an app does **not** require changing the Cobalt version or creating
a platform release.

## Build an app

```sh
cargo install --path crates/kobo-cli
kobo new my-app
cd my-app
kobo dev
```

`kobo dev` runs the app in the Clara BW browser simulator. Start with the
[SDK documentation](https://bandarlabs.github.io/Cobalt/sdk.html); the
repository also keeps the [deep implementation guide](SDK.md).

### What the SDK provides

| Area | Application-facing support |
|---|---|
| App model | Ordinary Rust binaries with declarative screens, named actions, lifecycle callbacks, and runtime-managed Back navigation |
| E-ink UI | Measured text, rows, tiles, pictures, dialogs, keyboards, terminal views, pagination, and full or partial refresh planning |
| Network and credentials | Asynchronous HTTPS fetches and posts, ranged downloads, bounded responses, and named secrets whose values never enter the app |
| State and background work | Atomic per-app keyed storage, cancellable tasks, foreground/background lifecycle events, and scheduled wake capabilities |
| Device and media | Capability-gated battery, cover, frontlight, Wi-Fi, Bluetooth, and audio requests |
| Tooling | App scaffolding, the browser and native runtime simulators, layout diagnostics, deterministic failure scenarios, packaging, and device deployment |

Apps request services through the SDK instead of opening device resources
directly. The runtime can deny a request because it was not declared, is too
expensive for the current battery state, or is unsupported, and each refusal
is returned to the app as a value it can present or recover from.

See the SDK docs for the
[application model](https://bandarlabs.github.io/Cobalt/sdk.html#application-model),
[UI components](https://bandarlabs.github.io/Cobalt/sdk.html#ui),
[runtime services](https://bandarlabs.github.io/Cobalt/sdk.html#services),
[capabilities](https://bandarlabs.github.io/Cobalt/sdk.html#capabilities),
[developer-facing crates](https://bandarlabs.github.io/Cobalt/sdk.html#crates),
and the [CLI command reference](https://bandarlabs.github.io/Cobalt/sdk.html#cli).

## Contributing apps

App contributions are regular pull requests:

1. Add the app as a workspace package under `apps/<app-id>/`.
2. Add its release metadata to `apps/catalog.json`.
3. Add unit tests and layout checks for every affected supported profile.
4. Run the app in the browser and runtime simulators.
5. Open a pull request.

After the PR is reviewed and merged, the `Publish apps` workflow builds every
registered app for ARM, signs the packages and catalog, and updates the fixed
Store channel. App versions are independent from the Cobalt platform version.

See [docs/CONTRIBUTING_APPS.md](docs/CONTRIBUTING_APPS.md) for metadata,
capabilities, testing, and release details.

## Repository layout

| Path | Purpose |
|---|---|
| `apps/` | Store applications and release registry |
| `examples/` | Built-in applications and SDK examples |
| `crates/kobo-sdk` | Public application SDK |
| `crates/kobod` | Device runtime |
| `crates/kobo-ui` | Layout and e-ink renderer |
| `crates/kobo-sim` | Clara BW browser/runtime simulator |
| `crates/kobo-app-store` | Signed package and catalog formats |
| `crates/kobo-cli` | Setup, build, simulation, packaging, and release tools |
| `docs/` | Installation, device, app publishing, and development guides |

## Development

```sh
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
cargo run -p kobo-cli -- run --sim --app pret-numerique
```

Additional guides:

- [Contributing](CONTRIBUTING.md)
- [Developing Cobalt](docs/DEVELOPING.md)
- [Working with devices](docs/DEVICES.md)
- [Publishing apps](docs/APP_STORE.md)
- [Porting to another Kobo](docs/PORTING.md)
- [Security policy](SECURITY.md)

## Safety and support

Cobalt does not replace Kobo's boot chain. Device support is explicitly gated
by hardware and firmware identity, and a reboot returns to the stock reader.
The first installation still modifies files on the user storage partition and
is provided without warranty.

Normal panel-write entry points require one of the exact hardware and firmware
combinations in the
[device support matrix](docs/DEVICES.md#device-support-matrix). Do not treat a
read-only profile match as permission to install: normal use requires the
profile's owner-attended display, touch, exit, and recovery evidence to be
complete.

## License

GNU Affero General Public License v3.0. See [LICENSE](LICENSE) and
[THIRD-PARTY.md](THIRD-PARTY.md).
