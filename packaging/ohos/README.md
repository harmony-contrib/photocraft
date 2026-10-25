# PhotoCraft on OpenHarmony

The native editor uses egui/eframe **0.36.2** (`feat-openharmony`), wgpu **30.0.1**, and
`winit` **0.30.13** (`feat-openharmony`) with
`winit::platform::ohos` and `openharmony-ability` (`385ce749121b56ea33226f8542e3f39afd6e5ed6`).
The Rust and ArkTS Ability packages
are built from the same revision (`OPENHARMONY_ABILITY_DIR` defaults to `../openharmony-ability`). On API 23+, `files-save.patch` requests that
the picker leave destination creation to Rust. The tested emulator nevertheless
creates placeholders; see the public-folder saving limitation below.

The ArkTS code only hosts the system Ability, XComponent and platform plugins.
The editor, rendering, tools, document formats and automation stay in Rust.
`apps/photocraft-ohos` isolates N-API's generated registration code; the editor
and engine retain their `unsafe_code = forbid` policy.

## Build and install

Prerequisites: Rust 1.95 or newer, `aarch64-unknown-linux-ohos`, DevEco Studio
with its OpenHarmony SDK and a running ARM64 simulator. The tested SDK is API 26.
The packaging target declares phone, tablet and 2in1; the regression record covers **2in1 and tablet**, with additional phone engine
and native pen checks. Phone layout changes are outside this adaptation.

```sh
rustup target add aarch64-unknown-linux-ohos
packaging/ohos/build-hap.sh
hdc install -r target/photocraft-hap/entry/build/default/outputs/default/entry-default-unsigned.hap
hdc shell aa start -a EntryAbility -b ai.storyteller.photocraft
```

`DEVECO_HOME` defaults to `/Applications/DevEco-Studio.app/Contents`.
`OHOS_NDK_HOME` can override the SDK directory containing `native/`.
The build uses the repository icon and system fonts, and writes generated
projects, native libraries and HAPs under `target/`. The unsigned HAP is for
developer simulators; device/distribution signing uses the generated DevEco
project and the developer's own signing configuration.

The OHOS renderer selects GLES, which is exposed by the emulator, and requests
the adapter's advertised device limits. The editor retains its CPU compositor
fallback and Performance rendering preference. File pickers run asynchronously
and persist the selected file grants. Image clipboard reads and writes are
ordered, and permission prompts never block the UI thread. Text uses the native
IME, UTF-16 selection/deletion and installed HarmonyOS fonts. Font discovery runs
on a startup worker while a loading view keeps the Ability responsive.

The first save into a public folder asks the user to select that containing
folder after choosing the file name. PhotoCraft needs this directory grant for
its temporary-file + fsync + rename save; a single-file picker grant cannot do
that. Select the exact containing folder, or cancel to leave the document
unsaved. Subsequent saves in that folder reuse the grant during the process lifetime. File and folder URI
grants are stored in the app's private directory and reactivated on launch.
A file opened from another folder requests the same authorization on its first
Save. No in-place, truncating-write fallback is used.

**Known blocker on the tested API 26 emulator:** public-folder atomic rename
returns `EACCES`, including after directory selection and an experimental
Documents-directory permission grant. ArkTS `fs.renameSync` reproduces it;
private-directory rename succeeds. Public-folder saving and save-after-restart
are therefore **not validated as working**. The experimental broad permission
and diagnostic probes are not shipped. See [VALIDATION.md](VALIDATION.md).

## Validation build

```sh
packaging/ohos/build-hap.sh --features ohos-control
hdc fport tcp:17878 tcp:7878
```

This opt-in feature enables the existing authenticated loopback control protocol
on port 7878. The build generates a local mode-0600 credential at
`target/ohos-validation/control.token` and embeds it in that validation binary.
Do not distribute the validation binary or credential. Ordinary builds have no
control listener. The automation read/write roots are the app's
`files/validation` directory; native user-selected files still use the system
picker. See [the control protocol](../../docs/control-protocol.md).

See [VALIDATION.md](VALIDATION.md) for the actual device checks and limitations.
