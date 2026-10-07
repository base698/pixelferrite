# Review fixes: verification guide

These checks cover the October 2026 security and correctness review. Use disposable images. The normal test suite uses local fake AI servers and makes no paid requests.

## Automated checks

From the repository:

```sh
cargo test --workspace --locked
cargo test --locked -p pixelferrite edge_detection_and_ai_edit
cargo install cargo-audit --version 0.22.2 --locked
cargo audit --deny warnings
./scripts/bundle-macos.sh --no-shortcut   # macOS only
```

The second command deliberately runs the formerly dependent UI test alone. UI tests drive the actual App with pointer/keyboard events and render through wgpu. Their screenshots are under `target/uitest/<test-name>-<pid>/`. They need a local graphics adapter; on Linux CI uses Mesa software Vulkan and Xvfb. Tests create private temporary settings/history instead of using your real account. The seven ignored demo/live tests are optional and are not part of acceptance; some make paid API calls.

The two-platform `Checks` workflow runs tests on macOS and Linux and builds a macOS app bundle. `Dependency advisories` checks the complete lockfile on dependency changes and weekly. Warnings, including unmaintained dependencies, fail the audit. The text engine now uses Skrifa; Linux window titles use FreeType/fontconfig.

## Run an isolated manual session

```sh
python3 scripts/make-verification-fixtures.py /tmp/pixelferrite-fixtures
PIXELFERRITE_CONFIG_DIR=/tmp/pixelferrite-check-config \
PIXELFERRITE_DATA_DIR=/tmp/pixelferrite-check-data \
cargo run --locked -p pixelferrite
```

Choose fresh directory names if those already contain a previous session. Use File > Open for the fixtures. No AI key is needed for checks 1–10 below. Tests for actual HTTP behavior are automated; entering a real key and pressing Send incurs the provider's normal charges.

1. **Input rejection without data loss.** Paint a mark in a small document and save it. Open `oversized-canvas.ora`; expect an explanatory error and the original image still open. Open `grouped.ora`; expect an unsupported-group explanation, with no modified rendering accepted. New Image at 16,384 × 16,384 must report the pixel limit and keep the current document. The oversized fixture contains only tiny PNGs, so this check never requires creating a huge image.

2. **Safe merge.** Open `merge-normal.ora`, select the white top layer and Merge Down. The picture must remain white; Undo and Redo preserve appearance. Open `merge-backdrop.ora` and try the same action. It must explain that the blend modes depend on the layers below and keep all three layers. Hidden or locked merge targets are likewise refused rather than revealed or altered.

3. **Preview and external edits.** Open `white.png`, start Gaussian Blur, drag `green.png` from Finder/the file manager onto the canvas, then Cancel. The green image must arrive after cancellation and remain as its own undoable layer. Repeat with Perspective. Repeat with Apply instead of Cancel. During a preview, history restoration and AI completion must also wait; the local mock-server regressions cover those paths.

4. **Stroke cancellation and redo.** Add a layer, Undo, start a brush stroke and press Escape while still holding the pointer down. Redo must still restore the added layer. Repeat starting a stroke outside the picture; the previous edit must not be undone. Closing the window during an unfinished edit must keep the window open and ask you to finish or cancel it first.

5. **Alpha and editable text.** Insert `transparent-edge.png` into a larger selection, then blur through a feathered selection. Its visible fringe must stay red, with no blue/purple contamination from the transparent pixel. Add text away from the top-left, add a mask, paint on the mask, then change the text. Its logical position must stay fixed through editing, Undo/Redo, and native save/reopen.

6. **Locked layers and mask Cut.** Lock a layer and try flipping, dragging it, editing its text, and changing X/Y in the inspector. Content/position remain unchanged. Unlock, select its mask and Cut a selection: the clipboard must represent mask coverage and only the selected mask pixels are erased. Paste produces a grayscale representation of that mask. Select Pixels to cut color content instead.

7. **Small scaling and background work.** Create a 3 × 3 image, choose Content-Aware Scale and request 1 × 1. Both image and canvas must reach 1 × 1. With a larger photo, start a slow filter or content-aware scale. The work indicator must appear and the window remain responsive. Cancel leaves pixels/history unchanged; changing documents after cancellation must never receive the stale result. Cancellation discards results immediately; the current computation can finish in the background before another heavy operation starts.

8. **Compatible export.** Create a red layer over white and hide half of it with a layer mask. Save normally, then choose File > Export Compatible ORA. Reopen both: both pictures match; the native file retains its editable mask, while the compatible copy bakes coverage into alpha and removes editable text metadata. For third-party verification, open the compatible copy in Krita/GIMP/MyPaint and compare the visible picture. Grouped imports remain explicitly unsupported.

9. **Crash recovery.** In the isolated session, paint a recognizable mark, release the pointer, and wait at least 35 seconds. Force Quit only this disposable session, then relaunch using the same config/data directories. Recover must restore the mark and mark the document unsaved. Force Quit again before saving; recovery must still be offered. Finally save, close normally, relaunch, and confirm no stale recovery is offered. Repeat with Discard Recovery. A second simultaneously running window must never offer another live window's snapshot.

10. **Save/discard handling.** Modify an image and close the window. Save, Discard and Cancel must be available. Cancel keeps editing; Save completes the save before closing; a cancelled save dialog or unwritable destination keeps the document open and reports failure. Discard must not leave an offer to recover that deliberately discarded image on the next launch. The automatic tests separately verify temporary-file collisions, symlinks and failures without touching your files.

11. **AI settings and privacy (no Send required).** Open Settings. Changing the endpoint must clear an existing saved key. Re-entering a key authorizes the new destination; the prompt shows the actual host. History retention accepts 0. Saving 0 removes retained requests and new requests keep no files. AI Requests > Clear All erases old records and in-flight requests cannot recreate them. On Unix, inspect the isolated config/data folders: private directories are `0700` and private files are `0600`. The key is permission-protected plaintext, not a keychain secret. The automated endpoint tests use dummy keys to prove `.env` cannot redirect a saved key and redirects cannot forward uploads.

12. **AI replay (optional paid manual check).** Make a small selection with feathered edges and request an edit. Duplicate the document for comparison, undo the AI layer, then restore it using AI Requests > Add Result as Layer. The restored pixels, transparency and placement must match the first insertion, including untouched pixels outside the selection. Older selected history without `result.png` must explain that exact restoration is unavailable instead of covering the context rectangle. The same assertions run automatically with a local fake answer, including responses arriving during a preview and history-off mode.

## Finding-to-test map

| Review finding | Primary automated evidence |
|---|---|
| 1. Oversized images/archive bombs | `io_security`: tiny canvas, header, XML/layer/entry/expansion and aggregate-pixel tests; `app_regressions::invalid_new_image_reports_error_without_replacing_document` |
| 2. Endpoint/key trust | `ai::tests::credentials_stay_with_their_approved_origin`, endpoint validation and redirect tests; settings reauthorization UI regression |
| 3. Merge appearance | `core_regressions`: supported-composite/undo and unsupported-merge tests; rejected-merge UI regression |
| 4. Preview rollback losing other edits | `app_regressions`: queued drops through filter/perspective cancel; `aiui::regressions::completed_answer_waits_for_preview_and_drag` |
| 5. Stroke cancel changing history | `app_regressions`: empty-stroke and redo-branch tests; core text/geometry cancellation test |
| 6. AI history losing selection | `aiui::regressions::history_restores_exact_clipped_pixels_and_position`, legacy-history refusal, request persistence tests |
| 7. Transparent RGB fringes | `core_regressions::selected_blur_is_invariant_to_invisible_rgb`, `image_scaling_is_invariant_to_invisible_rgb` |
| 8. Text-mask origin | `core_regressions::editing_a_text_mask_preserves_origin_and_future_text_edits` (including save/reopen) |
| 9. ORA group semantics | `io_security::grouped_and_effectful_stacks_are_rejected_without_changing_the_file` |
| 10. Snapshot memory budget | `core_regressions`: distinct raster, mask, selection and merged-history allocation tests with reduced test budgets |
| 11. Temporary-save symlinks | `io_security`: symlinks, simultaneous writers, abandoned/failed atomic writes; store path tests |
| 12. Private/optional AI history | `store::tests`: owner-only permissions and no-resurrection tests; mock-request history-off UI test |
| Locked edits, mask Cut, 1px scale | Corresponding `core_regressions` tests; locked inspector controls and move bounds checked through application tests |
| Test fixture dependency | `edge_detection_and_ai_edit` creates its own recent-file fixture and test-specific output folder; rerun it alone |
| Recovery and save consistency | `recovery::tests`: active sessions, cancelled writer, discard markers, bounded metadata, symlinks and second-crash tests; save XML validation test |
| Background work | App and geometry regressions cover preview completion/cancellation, revision mismatch and queued work |
| Parser robustness | `io_security::malformed_small_ora_and_png_mutations_do_not_panic` runs deterministic truncation/byte mutation cases |
| Font maintenance | Skrifa metric/layout regressions; complete lockfile audit has no unmaintained parser exception |

## Scope and remaining decisions

No feature expansion into RAW, color management, adjustment layers or shapes is claimed. Nested groups are safely rejected, not supported. Cancellation is result cancellation, not immediate interruption of algorithm threads. Input/history limits bound retained data but do not cap all transient/GPU memory. The mutation sweep is a regression corpus, not an exhaustive fuzzing campaign. Third-party editor fidelity and visible native OS dialogs need the manual checks above; offscreen tests do not substitute for every OS integration.

Project licensing remains unchanged pending the owner's choice. Packaging is repeatable and locally signed; notarized public distribution requires release credentials and a licensing decision. Neither a public license nor distribution identity is invented by this change.
