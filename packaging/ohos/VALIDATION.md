# PhotoCraft OpenHarmony regression

Date: 2026-10-10. PhotoCraft base: `e2bb2bd` (0.5.0).

## Environment

- DevEco **MateBook Pro / 2in1** ARM64 emulator, 3120 × 2080, scale 1.9.
- API 26, system `7.0.0.106(DEVC00E47R1P3)`.
- egui/eframe 0.36.2, wgpu 30.0.1, winit 0.31.0-beta.3;
  dependency revisions are in [vendor/README.md](../../vendor/README.md).
- GLES adapter: emulated Mali-G77, OpenGL ES 3.0, host Metal translation.
- HAP: `ai.storyteller.photocraft`, `EntryAbility`.
- Validation uses the opt-in, authenticated local control feature for document
  inspection and pixel assertions. Mouse, touch, keyboard and IME input are
  injected through the emulator's native `uinput` / `uitest`, and system dialogs
  are operated through their visible UI.

## Completed interactions

| Flow | Observed result |
| --- | --- |
| Install and fresh-process launch | Native Rust editor renders through wgpu/GLES. Adapter limits are used rather than an unsupported WebGL2 limit. |
| Font initialization | Font discovery moved off the Ability UI thread. A subsequent launch reached editor-ready in approximately 1.2 s with warm filesystem caches. The previously observed font-scan ANR did not recur. |
| New document | Native Ctrl+N opens the dialog; creates a 512 × 384 document. |
| Mouse drawing | A native mouse drag produces the expected black brush stroke; sampled pixel (256, 152) is `[0, 0, 0, 1]`. |
| Undo / redo | Native Ctrl+Z restores the sampled pixel to white; Ctrl+Shift+Z restores black. |
| Touch and text | Native touch starts a Type layer. Chinese input renders with installed HarmonyOS CJK fonts. |
| IME UTF-16 editing | Input `鸿蒙😀`, select-all/replacement and backspace preserve the string and delete the emoji as one character; Ctrl+Enter commits. |
| Image clipboard | Copy Merged → New from Clipboard creates a matching 512 × 384 pixel document. The asynchronous clipboard result is applied on a later frame. |
| Native format | `.pcraft` saves and reopens with the brush and editable Chinese Type layer. |
| PSD | Export/reopen preserves the editable Type layer and brush pixel. |
| PNG | Export/reopen preserves the image; the expected flattening warning is shown. |
| Additional native window | A real secondary native window renders the document; closing it leaves the main editor functional. |
| Window resize / maximize / resume | 1024 × 700 resize and system maximize redraw correctly; returning after minimize retains the document and sampled pixel. |
| Picker cancellation | Cancel returns to the editor without changing the document. |
| Native public-file open | Ctrl+O → system picker → a pre-seeded `.pcraft` fixture opens with all 3 layers and sampled pixel `[0, 0, 0, 1]`. This tests opening, not public-folder saving. |
| Ordinary package / restart | The package without `ohos-control` launches and reopens that public fixture through Recent after a process restart. No listener on 7878 and no embedded validation credential. |

## Blocking result: public-folder save

This public-folder regression remains **not fully passing** and was explicitly
excluded from the 2026-10-10/11 capability update. Save As into the system Documents
folder fails at atomic replacement with `Permission denied (os error 13)`.
The document stays open and its previous bytes are not overwritten. The native
picker may leave a zero-byte placeholder despite `autoCreateEmptyFile = false`.

Diagnostics on the same device isolated the following:

- Creating, writing, fsyncing and deleting a sibling temporary file succeed.
- Rust `std::fs::rename`, directory-handle rename via `cap-std`, and ArkTS
  `fs.renameSync` all fail in the public folder. Hidden and ordinary filenames,
  a newly created subfolder, and existing/nonexistent destinations were tried.
- The same native rename succeeds inside the app's private files directory.
- Explicit directory selection, read/write policy activation, and an experimental
  user-approved `READ_WRITE_DOCUMENTS_DIRECTORY` grant did not resolve the denial.
  Additional operation bits did not resolve it either and were removed.
- The OS persistence query returned false even after a successful persist call;
  read-after-restart succeeded through Recent, but writable-grant persistence is not claimed as verified.

An independent fresh installation (`ai.storyteller.photocraft.regression`,
ordinary binary, no previous grants) reproduced the same denial after native
Open → Ctrl+S → select Documents. Its registry contained only file-read mode 1
and directory read/write mode 3. `fresh-save-result.jpeg` records the failure.
The temporary app was then uninstalled.

A system-level limitation or policy issue remains to be resolved on this image,
or compared on another 2in1 image/device. It is not established which system
component causes the denial. No truncating/copy-over fallback was added. The
broad experimental permission and all native/Rust probes are removed from the
ordinary build. Public-folder Save → Open → Restart → Save is **blocked**.

## Automated checks

- `PHOTOCRAFT_LOCALE=en cargo test -p photocraft-ui-egui -p photocraft-text -p photocraft -p photocraft-ohos`:
  **1,387 passed**, 10 ignored across unit/integration suites.
- `cargo test -p egui -p egui-winit --lib`: **64 passed**.
- File-dialog regression after adding directory authorization: **7 passed**,
  including cancellation, mismatched answers and restoring the originating document.
- Export scaling integration regression: **1 passed**.
- `cargo xtask test-corpus -p photocraft-io`: **373 passed**, 2 ignored;
  pinned corpus integrity checks passed. The full all-package corpus run was
  interrupted during compilation; this is the completed IO corpus run.
- Host native check (library and binary), optional `eframe/glow` check,
  `cargo xtask layers` (30 crates), and `cargo xtask wasm`: passed.
- Host and OHOS clippy pass with existing `collapsible_match` and
  `nonminimal_bool` warnings allowed. Strict `-D warnings` finds pre-existing
  occurrences in `affinity/preview.rs`, `ui-egui/analysis_ui.rs`, `canvas.rs` and
  `transform_tool.rs`; they are not platform-adaptation regressions.

## Evidence

Local artifacts are under `target/ohos-validation/`: `interaction.jsonl`,
`type-current.jpeg`, `font-worker-type.jpeg`, `multiwindow.jpeg`,
`maximized-final.jpeg`, native picker screenshots, and per-process logs.
`native-open.jpeg` and `ordinary-recent-reopen.jpeg` record public-file reads;
the fixture was copied once from the private validation file by a temporary
native diagnostic, not saved through the failing editor Save As flow.
`probe-system.log`, `cap-probe-system.log` and `dir-permission-system.log` under
`target/ohos-port/` retain the rename-failure comparisons. Fourteen task-created
zero-byte placeholders were deleted; unrelated public files were left alone.
Build/test logs are under `target/ohos-port/`. Generated projects, copied SDK
font inspection files and test credentials are not source resources; no new
font binary is committed. The temporary smoke-test HAP project was removed. The validation TCP forward
and emulator screen-timeout override were restored after the final run.

Ordinary HAP SHA-256:
`f15d2318b702f1d5d80c669bdd8f57259d18acae78c4c6f3e45e54920004baa0`.

## Limits of this result

- The original results above cover the **2in1 emulator**. The added tablet and
  phone checks below extend this record; physical hardware remains untested.
- Colour bitmap Emoji support was subsequently added and verified below.
- Native accessibility registration is observed, but an end-to-end screen-reader
  session was not run. Pen pressure/tilt, a physical touchpad and external file
  drag-and-drop are not claimed as device-verified by this run.
- The unsigned HAP is intended for the developer emulator. Physical-device or
  distribution signing requires the developer's own signing configuration.


## Capability update — 2026-10-10/11

Ability updated to matched, locally patched Rust/ArkTS source at
`06e041fa81862410bee03b7856dc09a489f49639`, following GPUI's Ability adaptation.
The egui 0.36.2 integration now preserves native pen pressure/tilt/twist/eraser
metadata, cancels presses without spurious clicks, and retries slow IME startup
with a 2 s maximum backoff while focused. Window cursor/icon and native drag
preview/completion extensions are shared with the adjacent egui adaptation.

The Type renderer now supports **CBDT/CBLC and sbix bitmap glyphs** using bounded
PNG/BGRA decoding, bounded caching, premultiplied interpolation, document transforms
and colour conversion. `HMOS Color Emoji` participates in fallback. Emoji remain
editable Type layers in `.pcraft` and PSD. No fonts were added to source resources.

| Device / capability | Result |
| --- | --- |
| 2in1 colour Emoji | `鸿蒙 😀🌈` renders in colour; `.pcraft` and PSD reopen with three layers and editable Type text; PNG export succeeds with its flattening warning. `capabilities-2in1.png`. |
| Phone capability flow | Private `.pcraft`, PSD and PNG save/reopen, pixel assertions, synthetic brush and undo/redo passed. Existing dock visibility was temporarily toggled and restored for the test; no layout code was changed. This is engine/platform coverage, not approval of the narrow-screen layout. `capabilities-phone.jsonl`. |
| Tablet capability flow | Same file/text/pixel checks passed, with the colour Emoji screenshot inspected. `capabilities-tablet.png`, `capabilities-tablet.jsonl`. |
| Tablet native pen pressure | **OS UITest** `Driver.penSwipe`, not `ui.pointer`, drives the canvas. With an 80 px brush the 0.25 stroke bounds are `[114,95,88,65]`; the 0.75 stroke bounds are `[272,75,126,104]`. Both native injections succeeded and the width difference assertion passed. `native-pressure-tablet.png`, `tablet-native-pressure-result.json`. |
| Tablet native touch / IME | Touch creates editable Type; actual soft-keyboard taps compose `nihao`, select `你好`, then commit. The root surface resizes for the keyboard. `Screenshot_2026-10-11T0011*.png` and `interaction.jsonl`. |
| Tablet native text clipboard / deletion | User-authorized native paste inserts `鸿蒙😀`; native Backspace removes the complete final Emoji. Permission consent resumes the asynchronous paste. |
| Tablet native windows / lifecycle | A real child window displays the pressure/text document, native X closes it, and Home/resume retains PID 6379 and five document layers. `Screenshot_2026-10-11T0012*.png`. |
| Tablet ordinary HAP | The ordinary package launches after replacement and port 7878 is not listening. The temporary UITest module is removed. |
| Phone / 2in1 ordinary HAP | Both launch after replacement with no listener on port 7878. 2in1 restores both recovery documents, including the previous unsaved document. UITest modules and the temporary port forward are removed. |

The egui test HAP uses the official API 18+ UITest pen methods through `hdc shell aa test`.
Its phone WGPU, tablet WGPU/Glow and 2in1 WGPU/Glow logs all contain native egui Start,
Move and End at pressure **0.25 / 0.5 / 0.75 / 1.0**. `uinput -S` alone is documented
as touch-equivalent and cannot establish pressure coverage. Nonzero tilt, twist
and eraser selection remain unverified on a device because these UITest methods
do not accept those parameters; mapping and cancellation have unit coverage.

Updated checks:

- `cargo test -p photocraft-text -p photocraft-ui-egui -p egui-winit --lib`:
  **1,245 passed**, 11 ignored (87 text, 1,145 UI, 13 input integration).
- The opt-in real-font bitmap test passed separately with Apple sbix and the
  emulator's HMOS CBDT font, at 8/16/32-bit depths and transformed text positions.
- Full `cargo xtask test-corpus`: **2,280 passed**, 20 ignored across 98 suites;
  every pinned corpus integrity check passed. This supersedes the partial IO-only
  corpus result in the original report.
- Clippy passed with the same pre-existing `collapsible_match` and
  `nonminimal_bool` allowances; layering (30 crates) and WebAssembly checks passed.
- `cargo xtask perf --quick` completed. The 20-Type-layer size change measured
  1.53 ms command + 12.52 ms refresh (14.05 ms total). This is a current-run
  measurement, not a before/after speedup claim; unrelated existing 24 MP opacity
  and visibility scenarios still exceed their 16 ms targets.

The public-directory atomic replacement failure remains excluded and unchanged.
No new claim is made for physical hardware, end-to-end screen-reader interaction,
nonzero pen orientation, a native Move drop, or phone directory-picker completion.
Rotation was requested on tablet, but the captured app surface stayed landscape;
that attempt is not counted as a successful PhotoCraft orientation regression.
Latest build/test logs have the `ohos-capability-` prefix under `target/`.

Latest ordinary HAP SHA256:
`597551f38adcfa64595944f7b4254cae7148393ca334778987d9ab8c34562218`.

Cleanup removed the temporary extracted HMOS font, probe binaries, the empty
root Hvigor output directory, the nested Ability build cache (251 MB), and the
desktop drag-test file. Source licenses, regression evidence and reproducible
build/test inputs are retained. The temporary autosave interval was restored to
10 minutes after confirming recovery; no user document was discarded.
The 2in1 emulator is left running the ordinary build with the CJK/colour Emoji
Type document selected; `target/ohos-validation/2in1-ordinary-capabilities.png`
records the final visible state.
