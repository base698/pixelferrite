# Pixelferrite manual test plan

Use this checklist to check the installed app and the fixes from the project review. Record **Pass**, **Fail**, or **Not run** for each numbered check. This document is a plan, not a claim that a particular build has passed.

## Time and equipment

| Pass | Approximate time | What it covers |
|---|---|---|
| Quick smoke: 1–4 | 10–15 minutes after setup | Correct build, painting, save/reopen, selections, filters, undo |
| Review regressions: 5–14 | Another 40–60 minutes | Rejected files, merging, cancelled edits, transparency, text, locks, background work, recovery, privacy |
| Optional AI: 15 | 10–20 minutes plus service time | Live result placement, replay, retention and local discard |

Use macOS or Linux with a working graphics driver and file picker. A mouse makes the held-pointer tests easier. Test the packaged app on macOS; on Linux use the release executable. Python 3 is needed only to create the small synthetic fixtures. Building from source additionally needs the project's pinned Rust toolchain and the platform dependencies in the README. Build time is not included above.

The main checklist makes no AI requests and needs no real credentials. Keep settings, recovery, deliberate crashes and file-permission tests in the disposable session below. Do not use personal artwork, your normal settings folders or production API keys for these tests.

## Set up a disposable session

1. Save any work in your usual Pixelferrite window and keep it separate from this test. Open Terminal in the checkout containing this guide.
2. Run the following setup once. Keep this Terminal window open; it remembers the test folder and the exact process to stop during the recovery test.

   ```sh
   PF_REPO="$PWD"
   PF_SESSION="$(mktemp -d "${TMPDIR:-/tmp}/pixelferrite-manual.XXXXXX")"
   python3 "$PF_REPO/scripts/make-verification-fixtures.py" "$PF_SESSION/fixtures"
   mkdir "$PF_SESSION/results"
   printf 'Disposable test folder: %s\n' "$PF_SESSION"
   ```

3. Set the executable to the build you intend to test. For the macOS bundle built in this checkout:

   ```sh
   PF_APP="$PF_REPO/dist/Pixelferrite.app/Contents/MacOS/pixelferrite"
   ```

   To test the Desktop shortcut's build, use Finder's **Show Original** on that shortcut and set `PF_APP` to that app's `Contents/MacOS/pixelferrite` executable. If it is already the app above, no change is needed. On Linux:

   ```sh
   PF_APP="$PF_REPO/target/release/pixelferrite"
   ```

   If the executable is missing, use the build instructions in [VERIFICATION.md](VERIFICATION.md) first. Do not silently switch to an older app.

4. Define and run this launcher. It uses fresh settings/data folders, removes inherited AI credentials, and starts from a folder without a project `.env` file. It does not change your normal settings or shell environment.

   ```sh
   pf_launch() {
     (
       cd "$PF_SESSION" || exit
       exec env -u OPENAI_API_KEY -u OPENAI_BASE_URL \
         -u OPENAI_IMAGE_MODEL -u OPENAI_IMAGE_QUALITY \
         PIXELFERRITE_CONFIG_DIR="$PF_SESSION/config" \
         PIXELFERRITE_DATA_DIR="$PF_SESSION/data" \
         "$PF_APP"
     ) >>"$PF_SESSION/app.log" 2>&1 &
     PF_PID=$!
     printf 'Test process: %s; test folder: %s\n' "$PF_PID" "$PF_SESSION"
   }
   pf_launch
   ```

   Use `pf_launch` again whenever a check says to relaunch. Do not substitute a normal Desktop launch for those relaunches: it would use your ordinary settings and recovery folder.

Fixtures are intentionally small: four 64 × 64 color images, a 2 × 1 transparency edge, two three-layer merge documents, a grouped document, and an oversized-canvas document whose actual image data is tiny. Put all saved test documents and exports in `results`, leaving the input fixtures unchanged. On macOS, paste the test folder into Finder's **Go > Go to Folder…** to make drag-and-drop convenient.

In the steps below, menu paths refer to the menus inside the app. **Cmd** means Command on macOS and Ctrl on Linux. When entering a number in a draggable field, double-click its value to type it. Tool shortcuts work when a text field does not have keyboard focus; click the tool icon instead if necessary.

## Quick smoke pass

### 1. Confirm the build and About dialog

- [ ] Open **Help > About Pixelferrite**.
- [ ] Record **Version**, **Commit**, and **Source status**. Compare Commit with the latest expected commit in the delivery handoff, not a hash copied from this document.
- [ ] Click **Copy build info** and paste into your test notes. It must contain the same identity shown in the dialog.
- [ ] Close About and confirm normal editing still works. On macOS, also open the Desktop shortcut and check its About identity if that shortcut is part of the delivery; close that extra window before continuing the isolated tests.

**Expected:** the intended build is running. A delivered build from committed source should report clean source. An unknown commit, a different commit, or modified source should be recorded and resolved before treating results as acceptance of the delivered build.

### 2. Paint, undo, save and reopen

1. Choose **File > Open…** and open `fixtures/white.png`.
2. Choose **Paint** (`B`) and draw a black mark. Check that the title/top bar shows **Edited**.
3. Choose **Edit > Undo**, then **Edit > Redo**.
4. Choose **File > Save As…** and save `results/smoke.ora`. Confirm **Edited** disappears.
5. Open `fixtures/green.png`, then reopen `smoke.ora` from **File > Open Recent**.
6. Choose **File > Export PNG / JPEG / GIF…**, export `results/smoke.png`, and open the PNG.

**Expected:** Undo removes only the mark, Redo returns it, and both reopened images match the saved picture. Native save uses `.ora`; exporting a flat image does not replace the native document's save path.

### 3. Layers, selection and movement

1. Open `white.png`. Choose **File > Insert Image as New Layer…** and insert `red.png`.
2. Choose **Arrange** (`V`). Move the red layer enough to expose some white. Toggle its visibility checkbox in the layer list off, then on.
3. Choose **Elliptical Selection** (`O`) and draw a small ellipse. Choose **File > Insert Image into Selection…** and insert `green.png`.
4. Undo and Redo the insertion first, then choose **Select > Deselect**. Try **View > Zoom to Fit**, **Actual Size**, and **Reset Rotation**. Deselect has its own history entry, so doing it before Undo would undo the selection change instead of the insertion.

**Expected:** the red layer moves independently. The inserted green image is clipped to the ellipse, with no green rectangle outside it. Visibility and undo/redo preserve the other layers. View changes do not change the artwork.

### 4. Filter preview, Apply and Cancel

1. Open `smoke.ora` so there is a visible mark to blur.
2. Choose **Filter > Blur & Sharpen > Gaussian Blur…**. Adjust the radius, then click **Cancel**.
3. Repeat and click **Apply**. Undo and Redo once.
4. Choose **Filter > Stylize > Edge Detection…**; inspect its preview, then Cancel.

**Expected:** Cancel restores the exact pre-preview picture and adds no filter edit. Apply makes one undoable edit. The window remains usable and previews do not leave unexplained changes behind.

## Review regression pass

### 5. Reject unsupported or excessive inputs safely

1. Open `smoke.ora` and leave it saved, so an unsaved-changes prompt does not obscure the rejection test.
2. Try opening `oversized-canvas.ora`. Dismiss the error with **OK**.
3. Try opening `grouped.ora` and dismiss the error.
4. Choose **File > New…**, enter Width `16384` and Height `16384`, then **Create**.

**Expected:** each unsupported input produces an explanation and leaves the original picture open. The grouped file is rejected because nested groups are unsupported. The large dimensions are rejected before allocating that canvas; do not create your own enormous image or archive for this check.

### 6. Merge preserves appearance or explains why it cannot

1. Open `merge-normal.ora`, select the top **white** layer, and choose **Layer > Merge Down**. Undo, then Redo.
2. Open `merge-backdrop.ora`, select **white**, and try Merge Down.
3. Reopen `merge-normal.ora`. Hide the middle **gray** layer with its visibility checkbox, select **white**, and try Merge Down.
4. Reopen it again. Select **gray**, choose Arrange (`V`), and click **Lock** in the inspector. Select **white** and try Merge Down.

**Expected:** case 1 stays visually white and changes three layers to two; Undo restores three without changing the picture. The other cases explain the unsupported blend/hidden/locked target and keep the layers and appearance unchanged.

### 7. A file drop survives preview cancellation

1. Open `white.png` and start Gaussian Blur.
2. Drag `green.png` from Finder/the file manager onto the canvas while the preview is open.
3. Click **Cancel**, wait for any background work to finish, and check the layer list. Undo once, then Redo.
4. Repeat using **Apply** instead of Cancel.
5. Repeat both outcomes with **Filter > Geometry > Perspective…**. Move a corner slightly before dropping the file.

**Expected:** the drop waits until the preview ends, then adds a green layer. Cancelling the preview must not erase that layer. The insertion has its own undo step. While a preview owns the picture, other document-editing controls must not silently change it. AI completion/history insertion use the same boundary and have separate automated coverage.

### 8. Escape cancels a gesture without damaging undo/redo

1. Open `white.png`, paint a recognizable mark, and release the pointer.
2. Choose **Layer > New Layer**, then **Edit > Undo**. There is now a layer creation available to redo.
3. Start another Paint stroke inside the picture. While still holding the mouse button, press **Escape**, then release the button.
4. Choose Redo. Repeat the test with a stroke beginning outside the image in the canvas margin.
5. Open a filter preview and try closing the window. Cancel the preview afterward.

**Expected:** the cancelled stroke leaves no mark; the original mark remains; Redo still restores the new layer. An empty/outside stroke must not undo the previous edit. Closing during the unfinished preview keeps the window open and asks you to finish or cancel the edit first.

### 9. Transparent edges do not acquire blue/purple fringes

1. Open `white.png`. Draw a wide rectangular selection across its middle.
2. Choose **File > Insert Image into Selection…** and select `transparent-edge.png`.
3. Deselect and zoom in. The opaque red end should fade into transparency over white.
4. Draw an ellipse crossing that red edge and apply a modest Gaussian Blur. Deselect again.

**Expected:** the edge stays red or pale red against white; no blue/purple halo appears. The fixture deliberately stores blue in a completely transparent pixel, which must not leak into visible color. The UI has no separate Feather command; an ellipse supplies an antialiased selection boundary for this visual check. Fractional-mask cases also run in automated tests.

### 10. Text remains anchored, editable and correctly marked as changed

1. Choose **File > New…**, create a white `512 × 512` image, then choose **Type** (`T`). Click well away from the top-left and enter `Anchor 1` in the inspector's text box.
2. Save as `results/text.ora`. Change it to `Anchor 2` without switching tools or layers.
3. Confirm **Edited** reappears. Use the app's **Edit > Undo** and **Edit > Redo** menu commands, then Save again. Use the menus rather than clicking the canvas to leave the text box: clicking the canvas with Type can create another text layer.
4. With no selection, choose **Layer > Add Mask**. Click the mask thumbnail (the second thumbnail in that layer row), choose Paint, use black, and hide part of the text.
5. Return to Type and change only the final digit. Compare the unchanged `Anchor` prefix with its earlier position. Undo/Redo the edit.
6. Save, close/reopen the native `.ora`, and edit the final digit again. Also try a system font and **Draw a Path to Follow**, draw a short curve, and edit the text on it.

**Expected:** changing saved text marks the document edited and remains undoable. Painting the mask does not rasterize the text or move its logical anchor; later text edits and save/reopen keep the same baseline/path placement. The layer continues to be labeled **Text**. Different letters can have different visible bounds, so compare the unchanged prefix rather than expecting identical raster X/Y for every string.

### 11. Locks, mask Cut and extreme position fields

1. Use the text document from check 10. Select the text layer, choose Arrange, and click **Lock**. Try dragging the layer, editing its X/Y values, **Layer > Flip Horizontal**, and changing its text using Type.
2. Unlock it. Open `white.png`, insert `red.png`, and choose **Layer > Add Mask** with no selection.
3. Click the mask thumbnail. Draw a rectangle over half the layer and choose **Edit > Cut**. Deselect, then **Edit > Paste as New Layer**.
4. Hide or delete the pasted layer to inspect the original. Click the original layer's pixel thumbnail/row to return to pixels, make a selection, and try Cut again.
5. On an unlocked disposable layer, use Arrange's X/Y fields to enter an excessive value such as `999999`. Save/reopen a copy afterward.

**Expected:** locked content and position do not change. Cutting the mask changes only mask coverage; pasted mask content is grayscale coverage rather than red image pixels. Cutting pixels changes color content instead. Position fields clamp to supported bounds (currently ±65536), and the document remains saveable. Off-canvas content at a large position is expected.

### 12. Tiny scaling, slow work and retained undo history

1. Create a white `3 × 3` image. Choose **Filter > Geometry > Content-Aware Scale…**, enter `1 × 1`, and **Apply**. Check the layer dimensions with Arrange and inspect the canvas; Undo once.
2. Create a disposable `1024 × 768` image and paint several marks. Start **Filter > Repair > Reduce Noise…**. While its work indicator is visible, move the window or use the zoom controls, then click **Cancel Operation**. This closes the filter preview as well as discarding its work. If the worker finishes before you can cancel it, click **Cancel** in the remaining filter dialog first, then retry with a modestly larger disposable image.
3. Open `green.png` after cancellation. Wait until the background indicator disappears. Repeat with Content-Aware Scale, requesting a noticeable reduction and cancelling while it runs.
4. On a small image, make several separate strokes, selection changes and mask edits. Watch **History** and its **MiB retained** value. Undo/Redo and click earlier history entries.

**Expected:** both layer and canvas can reach `1 × 1`, and Undo restores the small image. Slow work runs behind a responsive interface; cancelling applies no pixels/history change, and a late answer never overwrites the newly opened green image. **Cancelled; finishing background computation** can remain briefly: cancellation discards the result but does not instantly stop the computation. Another heavy operation may ask you to wait. History reports retained raster memory and navigates the retained steps correctly. The exact 200-step/1-GiB eviction boundary is tested automatically with reduced budgets; do not fill memory just to prove it manually.

### 13. Native/compatible saves and unsaved-change decisions

1. Open white, insert red, select half the red image and choose **Layer > Add Mask**. Deselect. Optionally add editable text on another layer.
2. Save `results/native.ora`, then choose **File > Export Compatible OpenRaster…** and save `results/compatible.ora`.
3. Reopen both files. If available, open the compatible copy in Krita, GIMP or MyPaint as a separate interoperability check.
4. Open `native.ora`, paint a mark, and close the window. Choose **Cancel**; the window must stay open. Close again and choose **Save**; relaunch, open `native.ora`, and check the saved mark.
5. Create an unsaved image, draw a mark, close, choose Save, then cancel the file picker. The app must remain open. Close once more and choose **Discard**, then relaunch.
6. Optional failure check, only in this disposable session: create a folder inside `results`, make it read-only, and attempt Save As into it. Restore its permissions afterward. The OS picker may reject the location before the app can try saving.

**Expected:** both ORA files look the same. The native copy keeps its editable mask/text; the compatible copy bakes the mask into alpha and text into pixels. Save completes before closing; cancelling or failing a save keeps the document open and edited. A refused destination is explained by the picker or app. Deliberately discarded work must not return as a recovery offer. Exporting does not count as saving the editable document.

### 14. Recovery and private settings — isolated session only

#### 14a. Recover after a deliberate crash

1. In the launched disposable process, make a recognizable unsaved mark. Release the pointer and leave the document idle for at least 35 seconds.
2. Check `data/recovery/<session>/recovery.ora` inside the printed test folder exists before crashing; allow more time if the writer is still finishing. A filter/settings dialog left open can delay a snapshot.
3. In the setup Terminal, confirm the printed `PF_PID` is this disposable app. Run `kill -KILL "$PF_PID"` to simulate a crash of **only that process**, then `pf_launch`.
4. Choose **Recover** in **Recover unsaved work**. Check the mark and **Edited** status. Before saving, deliberately stop this same test process again and relaunch: recovery must still be offered.
5. Recover, save a native file in `results`, close normally, and relaunch. There should be no stale recovery offer.
6. Repeat with a fresh unsaved mark, wait for its snapshot, stop/relaunch, and choose **Discard Recovery**. Relaunch again and confirm it stays discarded.
7. Optional two-window check: while one disposable process with a snapshot is still running, save its PID in `PF_FIRST_PID="$PF_PID"`, then run `pf_launch` to start a second isolated process using the same folders. The second process must not offer the live first process's snapshot. Close both normally; retain the separate PIDs if you need to identify them.

**Expected:** completed snapshots survive crashes, including a second crash immediately after recovery. Saving/discarding clears obsolete recovery. Live windows cannot claim each other's snapshots. Do not use Force Quit All, `killall`, or a process name for this test.

#### 14b. Endpoint trust without sending anything

1. Open **File > Settings…**. Leave **API URL** empty, enter dummy **API key** `manual-fixture-not-a-real-key`, set **Keep** to `200`, and Save.
2. Open **Layer > Send to AI with Prompt…**. Enter a harmless prompt if needed. Check the upload destination is `https://api.openai.com` and the key source is Pixelferrite settings. Click **Cancel**, never Send.
3. Reopen Settings and change API URL to `https://example.invalid/v1`. The old key must clear immediately. Enter a different dummy key and Save. Reopen the AI prompt: the destination must now be `https://example.invalid`. Cancel.
4. In Settings, try `http://example.invalid/v1`. Save must explain that remote HTTP is not allowed and keep the dialog open. Localhost/loopback HTTP is reserved for development fixtures. Cancel these changes.
5. Restore empty API URL, re-enter the original dummy key, and Save. In the test folder only, create a `.env` containing the lines below, reopen the AI prompt, and inspect the destination/source again:

   ```text
   OPENAI_API_KEY=untrusted-fixture-key
   OPENAI_BASE_URL=https://collector.invalid/v1
   ```

**Expected:** a saved key cannot be silently redirected by the local file; the prompt still identifies the saved key source and official OpenAI destination. No request is made. Delete that test `.env` afterward. Credentials remain permission-protected plaintext, not encrypted keychain entries.

#### 14c. Clear/disable history and reject unsafe legacy replay without an API call

Keep must be `200` for the first part. With only the disposable app running, this snippet seeds an intentionally old selected-history record using the tiny green fixture; it sends nothing. Files created by this snippet are test data, not evidence of the app's file-creation permissions.

```sh
python3 - "$PF_SESSION" <<'PY'
import json, os, pathlib, sys
root = pathlib.Path(sys.argv[1])
record = root / 'data/ai/manual-selected-history'
record.mkdir(parents=True, exist_ok=True, mode=0o700)
os.chmod(record.parent, 0o700)
os.chmod(record, 0o700)
data = {'id': record.name, 'version': 1, 'selection': True, 'status': 'done',
        'prompt': 'Disposable legacy history fixture — no upload'}
(record / 'request.json').write_text(json.dumps(data))
(record / 'output.png').write_bytes((root / 'fixtures/green.png').read_bytes())
for path in record.iterdir():
    os.chmod(path, 0o600)
PY
```

1. Open **Layer > AI Requests…** (close/reopen it if it was already visible). Select the fixture.
2. **Add Result as Layer** must be unavailable for this selected legacy record; its explanation says that the saved clipped layer is missing. **Show Files** must still expose its raw output. It must never insert an unclipped green rectangle.
3. Click **Clear All History**. The list and its saved request folder must disappear.
4. Run the seed snippet again. In Settings set Keep to `0`, then Save. Reopen AI Requests and check the fixture is gone. Reopen the AI prompt and check **History is off** is shown; Cancel.
5. Inspect the app-created `config`/`data` directories and `config/config.toml` with your file manager's permissions view or `ls -ld` / `ls -l`. On macOS/Linux, those directories must be owner-only (`drwx------`, mode 0700), and the settings file owner read/write (`-rw-------`, mode 0600).

**Expected:** clearing removes saved records; saving Keep=0 removes retained requests and disables future recording. Restarting with Keep=0 does not revive them. Automated local-server tests verify that a request finishing after Clear All also cannot recreate files, that history-off requests write no images, and that app-created history files have private permissions.

## Optional AI checks

### 15. Live insertion, replay and discard

This is optional and can incur provider charges. Prefer the existing automated local-server regressions if you only need regression confidence. For a live check use synthetic pictures, an approved disposable test credential and the isolated settings/data folders. Do not enter a production key or use private artwork. Each additional Send is a separate request.

1. Set Keep to `200` before the replay check. Configure the intended API destination and test key, then verify the host shown in the prompt before sending.
2. On a small white image with colored marks, draw an ellipse. Choose **Layer > Send to AI with Prompt…**, leave **Everything visible** selected, and request a simple change inside the selection.
3. While it runs, open a filter preview and leave it open until the answer should be ready. The AI layer must not insert into the active preview. Cancel/Apply the preview and allow the answer to insert afterward. Do not replace or close this document while waiting.
4. Export `results/ai-first.png` as a visual reference. Undo the AI insertion, Deselect, then open **Layer > AI Requests…**, select the request and click **Add Result as Layer**. Export `results/ai-replay.png` and compare at the same zoom.
5. Check **Show Files**: enabled history includes `request.json`, `input.png`, a `mask.png` for a selection, raw `output.png`, and the exact clipped `result.png`.
6. Optional additional requests: while one runs, use **Clear All History**; after completion its record must stay deleted. With Keep=`0`, another request may add a result to the document but must create no AI history files. Recovery snapshots of the edited document are separate from AI request history.
7. Optional additional request: click **Discard when ready**. No result should be inserted into the document. The HTTP request may still complete and be billed; if history is enabled, its completed result can remain there. The control does not promise to cancel the provider's work.

**Expected:** the AI result is a separate undoable layer, clipped to the original selection. Replay in the same document/canvas matches its first insertion, including position and transparency outside the ellipse. The raw provider response may include context; replay uses the saved clipped layer. If a document fills up while a request runs, the ready result stays in memory with **Add Result** / **Discard Result** so capacity can be freed before retrying; the automated test exercises that limit without paid calls.

## What manual checks do not prove

The checklist covers visible behavior; it does not replace the automated checks in [VERIFICATION.md](VERIFICATION.md). Archive expansion/header limits, malformed-input mutations, concurrent atomic saves, hostile symlinks, exact memory-budget eviction, redirect/upload blocking, in-flight deletion races and exact pixel equality are exercised there with bounded local fixtures. Do not create archive bombs, attack real servers, exhaust memory or alter permissions on personal folders for this manual pass. GPU/transient memory and third-party editor behavior still depend on the environment.

## Record results and report a bug

Copy this into your notes for each run:

```text
Date / tester:
Build info (paste from About > Copy build info):
Expected commit from delivery handoff:
OS and version / architecture:
GPU or software renderer (if known):
App path (Desktop shortcut, bundle or executable):
Disposable session folder:

1–4 quick smoke: Pass / Fail / Not run; notes:
5–13 regression checks: individual numbers and outcomes:
14 recovery/privacy: Pass / Fail / Not run; notes:
15 optional AI: Pass / Fail / Not run; provider/model if used:
Third-party ORA comparison: app/version or Not run:

Failure title / checklist number:
Starting fixture and exact steps:
Expected result:
Actual result and full error text:
Reproduces: always / sometimes (count):
Screenshot or short recording:
Relevant app.log excerpt and disposable .ora/PNG, if needed:
```

Attach only synthetic test artifacts. Do not include an API key, `config.toml`, private source images or an entire AI history folder. Screenshots of Settings must hide even a test credential.

When finished, close every disposable window normally. Keep `results`, the build identity and relevant logs until failures are resolved. Then remove only the printed disposable folder if no longer needed; leave normal Pixelferrite settings/data untouched. A complete acceptance record identifies the build and explicitly marks skipped checks, especially optional live AI and third-party interoperability.
