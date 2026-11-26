# UI snapshots

Renders every page of the app offscreen to PNG files, at desktop (1280 × 820) and phone
(390 × 844) sizes, in dark and light themes, plus a few interactions (the add-pedal menu, a knob
drag, the F1 shortcuts window). Use it to look at UI changes without running the app with live
audio. It uses `egui_kittest` with wgpu, so it needs a GPU driver (Vulkan or GL) but no window.

```bash
cd tools/ui_snapshots
cargo run                                # all shots into snapshots/
cargo run -- /tmp/shots tuner            # only shots whose name contains "tuner"
GVA_SNAPSHOT_TAB=/path/song.gp5 cargo run -- snapshots tabs_loaded
```

On Linux the build needs the ALSA headers, like the app.

The app runs with no audio devices, and the tool points `XDG_CONFIG_HOME`/`XDG_DATA_HOME` at a
temporary folder, so your real settings and presets are never read or written. This folder is
its own Cargo workspace (it isn't a member of the root one), so `cargo build --workspace` at the
repo root doesn't compile wgpu.
