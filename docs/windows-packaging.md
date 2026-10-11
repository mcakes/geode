# Windows packaging planning

Geode should support a per-user Windows installation with frequent updates
delivered through the company intranet. Bundle the application, background
collector, and vendor DLLs as one tested release. Velopack is the leading
candidate for installation and updating, subject to a trial on a managed
Windows machine under an ordinary employee account.

Status as of 2026-10-11: planning only. The repository has Windows CI, but
the packaging and update integration described below has not been built.
Tool behavior and company policies should be checked again when implementation
starts.

## Deployment requirements

- Distribution is within one company with a shared intranet.
- Windows is the deployment priority; development can continue on macOS.
- Releases will be frequent, especially during early development.
- Users generally cannot install into `Program Files`; per-user installations
  tend to be permitted. Initial installation and routine updates should work
  without elevation.
- Solace and the pricing library will introduce DLL dependencies. Their exact
  versions, runtime requirements, and complete dependency lists remain to be
  established.

Install and execute the application locally. Use the intranet to distribute
releases and shared desk configuration. This keeps program startup and loaded
DLLs independent of an executable directory on a network share.

## Packaging candidate

Velopack provides an official Rust integration with update checking,
downloading, and application restart. Its Windows `Setup.exe` installs under
`%LOCALAPPDATA%\{packId}`. It owns an `Update.exe` and a `current` directory
whose contents are replaced during updates. See the
[Rust integration](https://docs.velopack.io/getting-started/rust) and
[installer documentation](https://docs.velopack.io/packaging/installer).

The Rust SDK supports an HTTP update source and a `FileSource` for a local or
network directory. A static intranet web server or file share can therefore
host releases without a custom update service. Choose the transport after
checking access from employee machines, including authentication, proxy, and
certificate requirements. See the
[Rust update sources](https://docs.rs/velopack/latest/velopack/sources/index.html)
and [FileSource](https://docs.rs/velopack/latest/velopack/sources/struct.FileSource.html).

Alternatives remain useful if company deployment requirements change:

| Option | When to consider it |
|---|---|
| [Inno Setup](https://jrsoftware.org/isinfo.php) | A conventional installer with shortcuts and uninstall; the update mechanism needs separate design. It supports [non-administrative installation](https://jrsoftware.org/ishelp/topic_setup_privilegesrequired.htm). |
| [WiX MSI](https://docs.firegiant.com/wix/) | IT prefers to own installation and updates through its existing deployment tools. |
| [MSIX with App Installer](https://learn.microsoft.com/en-us/windows/msix/app-installer/app-installer-file-overview) | IT supports MSIX and Windows-managed updates from an intranet server or share. Validate collector startup and packaged filesystem behavior first. |
| Versioned archives with a custom launcher | Existing company infrastructure makes this advantageous. Geode would own verification, activation, recovery, and launcher maintenance. |

Frequent releases favor an existing updater such as Velopack. A custom
launcher is a fallback, not an implementation commitment.

## Installation and persistent data

Use a package identity that gives the installer its own directory. For example,
`Company.Geode` below is a placeholder, not a chosen identifier. This is a
partial layout; Velopack also maintains its own metadata and package files.

```text
%LOCALAPPDATA%\Company.Geode\       Installer-owned program files
    Update.exe
    current\
        geode.exe
        geode-collector.exe
        <Solace DLLs>
        <pricing library DLLs>
        <additional runtime files>

%LOCALAPPDATA%\Geode\geode.duckdb   Existing default database location
%APPDATA%\geode\                   Existing personal configuration location
```

Avoid choosing `Geode` as the default package ID while the database lives at
`%LOCALAPPDATA%\Geode`: it would put persistent data beneath an installer-owned
root. Preserve user configuration and data through upgrades and ordinary
uninstall.

Shared desk configuration already comes from `GEODE_DESK_CONFIG`, which can
name a UNC directory. Personal overrides remain in the user layer. Ensure
the chosen setup provisions the desk configuration location for both the app
and collector. These are existing behaviors in
[path resolution](../crates/geode-compose/src/paths.rs) and the
[configuration guide](current/configuration.md); the database location can
also be overridden by `data.db_path`.

## DLLs and runtime dependencies

Treat the executables and vendor libraries as one release. Record the Geode
version and commit, architecture, vendor library versions, and packaged file
hashes so a reported problem can be tied to the actual installed combination.
Changing a vendor DLL produces a new release.

For ordinary native DLLs, begin with application-local deployment alongside
the executables. Solace explicitly documents this arrangement for its C API.
Include transitive dependencies: depending on the SDK and linking choices,
Solace may also need OpenSSL libraries. The same dependency audit is needed
for the pricing library. See
[Solace run guidelines](https://docs.solace.com/API/API-Developer-Guide/Run-Guidelines.htm)
and [compile guidance](https://docs.solace.com/API/Messaging-APIs/Code-and-Compile-Guideli.htm).

The tentative target is `x86_64-pc-windows-msvc`, conditional on vendor
support. Confirm whether the pricing DLL exposes a native interface or
requires .NET, COM registration, or another runtime before assuming it can
simply be copied. Match the architecture and ABI of every native dependency.

If Visual C++ runtime libraries are required, prefer an appropriate runtime
already maintained by company IT. Supported application-local redistributable
files are another option; their updates then become Geode's responsibility.
A global runtime installer must not become an unexpected administrator prompt
inside an otherwise per-user install. Microsoft documents both approaches in
[Redistribute Visual C++ files](https://learn.microsoft.com/cpp/windows/redistributing-visual-cpp-files).

The current [real source guide](current/real-sources.md) still describes
simulators and mock pricing implementations. Existing default-feature CI
does not prove that the eventual vendor-enabled package runs on a clean
employee machine.

## Proposed update policy

Download updates in the background and apply them at the next normal launch
or through an explicit update-and-restart action. Avoid interrupting a trading
session. Failed update checks or downloads should leave the installed release
launchable; this does not remove the application's need for its data sources.

Use two release channels:

| Channel | Audience | Release policy |
|---|---|---|
| Pilot | Developer and a few early users | Frequent candidates for feedback |
| Stable | Wider desk | Builds promoted after pilot use |

Promote the tested binaries without rebuilding them. Validate the packaging
tool's channel and promotion mechanics before defining publication commands.
Publish complete packages before making the feed advertise them, and verify
the downloaded release before activation. Use company-accepted code signing
for the installer, updater, and Geode executables, while preserving vendor
signatures.

Retaining old packages helps recovery, but a previous executable may not be
compatible with data written by a newer release. Republishing previous
working code under a higher version is one recovery option, subject to the
same compatibility constraint. Geode has a `STORE_FORMAT` stamp and does not
automatically migrate existing payload tables. Define a migration or deliberate
store-rebuild procedure before shipping incompatible changes; do not treat
binary rollback as database rollback. See the
[store format contract](current/data-path.md#the-store-format-stamp).

## Background collector lifecycle

The collector makes updating more involved than closing the app window. Its
Windows installation currently registers an absolute executable path in a
per-user Task Scheduler job, using `InteractiveToken` and `LeastPrivilege`.
Company policy may still restrict registration. Collector startup remains
opt-in; installing or updating Geode must preserve that choice. See the
[collector README](../crates/geode-collector/README.md) and
[task definition](../crates/geode-collector/src/install.rs).

The updater integration must stop and wait for an enabled collector, prevent
it from restarting during replacement, activate the matching app and DLLs,
preserve or update its registered path, and restore its prior enabled state.
Handle failure without leaving a task pointing at missing files. A stable
path under `current` may avoid changing the task command, but its behavior
during replacement needs testing.

The current reinstall path uses `schtasks /End`, which does not perform the
collector's release drain; pending coalesced documents can be lost and are
recovered on the next subscription. Choose an explicit shutdown and recovery
policy for updates. Velopack's general update support does not establish that
this Geode-specific lifecycle works.

Repository guidance reserves real collector registration for the owner.
Agents use dry-run plans and pure builders; the Windows installation trial
must be run by the owner.

## Windows build and release validation

Build release artifacts on Windows while continuing development on macOS.
The existing [CI workflow](../.github/workflows/ci.yml) checks both platforms
but does not package, sign, or publish releases. An internal Windows runner
may be appropriate for proprietary SDK access and intranet publishing.

Create a dedicated staging directory containing only distributable program
files and dependencies. Enable shared vendor source adapters explicitly in
both the app and collector; their shared sources must remain equal. The
pricing implementation remains in the app because the collector does not price.
Pin the packaging tools and SDK versions. Archive release symbols separately
for diagnosis. The Rust, C++, and packaging SDKs belong on the build runner;
client machines need only the application's runtime dependencies.

The first implementation milestone should be two small releases exercised
on a representative managed Windows machine as an ordinary employee:

1. Install the signed package without elevation or developer tools present.
2. Load the actual vendor DLLs, connect to Solace, and perform a pricing call.
3. Update through the intranet with a changed DLL and an enabled collector;
   verify both processes use the new release and the collector survives logon.
4. Repeat with collector startup disabled and ensure it stays disabled.
5. Interrupt a download or make the update feed unavailable and confirm the
   installed version remains usable. Exercise recovery from an apply failure.
6. Verify configuration and database preservation through update and uninstall,
   and exercise the selected recovery policy with compatible store data.

Resolve the actual employee execution policy, signing trust, scheduled-task
permissions, feed authentication, vendor dependency inventory, and store
compatibility policy before committing to the packaging design. If IT must
provision a runtime or publisher allowance once, keep routine Geode updates
independent of administrator access.
