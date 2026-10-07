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

## Manual acceptance testing

Use [the manual test plan](MANUAL_TEST_PLAN.md) for a quick smoke test, detailed
review regressions, disposable-session setup, and optional AI checks. Each case
includes steps and an expected result using the current menu labels.

Start with **Help > About Pixelferrite** and **Copy build info**. Compare the full
commit to the build you were given and check **Source status: Clean** before
recording results. The same information is available with `pixelferrite --version`.
A build with local changes or unavailable Git metadata is explicitly labelled.

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
