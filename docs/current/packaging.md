# Windows packaging

Geode has an initial Velopack package build for Windows x64. It produces a
per-user installer, portable archive, update feed and packages, and a separate
debugging-symbol archive. These are unsigned development artifacts using the
current mock adapters. Signing, intranet publication, vendor-enabled builds,
and automatic update installation still require integration.

The package ID is `Geode.Desktop`, giving the default installation root
`%LOCALAPPDATA%\Geode.Desktop`. This keeps installer-owned files separate from
the existing database at `%LOCALAPPDATA%\Geode\geode.duckdb` and personal
configuration at `%APPDATA%\geode`. The installer does not provision
`GEODE_DESK_CONFIG` or register the optional background collector.

## Building a package

Run from a Windows x64 development machine with PowerShell 7, the Rust MSVC
toolchain, Visual Studio C++ build tools and Windows SDK, and the .NET 8 SDK.
The .NET SDK runs the packaging tool on the builder; it is not a Geode client
prerequisite.

```powershell
./scripts/package-windows.ps1 -Version 0.1.0-pilot.1
```

The script restores the repository-local `vpk` tool pinned in
[dotnet-tools.json](../../.config/dotnet-tools.json), builds `geode.exe` and
`geode-collector.exe` with `--release --locked` for
`x86_64-pc-windows-msvc`, and stages a dedicated payload before packing it.
The Rust SDK and `vpk` are pinned to `1.2.161`; update them together after
checking compatibility. The build sets `GEODE_GIT_HASH` for the existing
store writer-build diagnostics.

Supply a new SemVer version for each release. `-Channel` accepts `pilot`
(default) or `stable`. A stable-channel package is still unsigned unless
signing is added; channel selection does not certify release readiness.

```powershell
./scripts/package-windows.ps1 -Version 0.1.0-pilot.2 `
    -Channel pilot -RuntimeDllDirectory C:\SDKs\geode-runtime
```

`-RuntimeDllDirectory` supplies explicitly selected runtime DLLs. Keep it a
flat deployment directory containing the complete required dependency set,
including any vendor TLS or C++ runtimes; it is not an SDK directory. The
script rejects missing or empty requested DLL directories and reserved file
collisions. It does not discover native dependencies or prove their ABI and
architecture compatibility. Headers, import libraries, build directories, and
debug symbols are not part of the application payload.

No framework bootstrap is requested from Velopack: the installer must not
silently require an elevated global runtime installation. Validate runtime
dependencies on a managed employee machine and coordinate any prerequisites
with IT. See [packaging planning](../windows-packaging.md) for those decisions.
The script rejects ambient `VPK_*` options so inherited framework, signing,
or installer settings cannot change this build contract. It supplies the
packaging arguments explicitly; remove those options from the builder's
environment before running it.

## Artifacts and CI

By default the output is beneath `target/packaging/<channel>/<version>`.
`-OutputDirectory` selects another new directory. An existing output directory
is refused so releases cannot accidentally be mixed or overwritten.

| Output | Contents |
|---|---|
| `payload/` | Both executables, supplied runtime DLLs, and release metadata |
| `releases/` | Velopack installer, portable archive, packages and feed files |
| `release.json` | Package version, channel, target, commit and staged file hashes |
| `symbols.zip` | Debugging symbols kept outside the distributable payload |

The [Windows package workflow](../../.github/workflows/windows-package.yml)
runs for packaging-related pull requests and can be dispatched with a version
and channel. It uploads installer/feed artifacts and symbols separately. PR
versions are generated from the workflow run and PR number. The workflow does
not install the package, register a collector, publish a GitHub Release, or
upload to an intranet feed. Proprietary DLLs are not available on the hosted
runner; supply them through a controlled build environment when their adapters
are implemented.

## Startup and update behavior

`geode-app::main` runs `VelopackApp` first on Windows. Install, update,
obsolete-version, and uninstall callbacks exit before Geode's logging,
argument parsing, store, or window initialization. Ordinary development
launches outside a package continue into normal app startup. Other platforms
do not depend on the Velopack SDK.

The SDK's default automatic application of staged packages is explicitly
disabled with `set_auto_apply_on_startup(false)`. There is no feed polling,
download operation, or update action in Geode yet. These must be added with
collector shutdown and restart coordination and a compatible store policy.
Existing downloads must not cause an uncoordinated upgrade merely because
the application starts.

The collector remains independently opt-in. Packaging it does not register
its scheduled task. Its registration holds an absolute path and its process
can outlive the UI. Before manually replacing or uninstalling a development
installation with a registered collector, the owner must unregister it using
the [collector procedure](../../crates/geode-collector/README.md), then opt in
again from the new installation if desired. Installer-driven preservation of
this registration is not implemented.

## Verification

```powershell
pwsh -NoProfile -File scripts/test-package-windows.ps1
cargo test -p geode-app --test velopack_startup
```

The package script tests exercise staging and tool failures without compiling
or installing the application. They run in Windows CI. The Rust integration
test runs only on Windows and launches the real binary with each Velopack
callback, checking that it exits successfully without creating user data or
logs. It never invokes Task Scheduler. The mutation harness has a Windows-only
entry for this startup boundary.

A managed Windows trial is still required for actual installer execution,
standard-user permissions, native library loading, collector lifetime,
upgrade and uninstall behavior, and company signing/application-control
policy. Automated checks of the scaffolding do not establish these outcomes.

The upstream [Rust integration](https://docs.velopack.io/getting-started/rust),
[packaging guide](https://docs.velopack.io/packaging/overview), and
[Windows installer behavior](https://docs.velopack.io/packaging/installer)
describe the tooling; the pinned source and local command help are the API
authority when extending this integration.
