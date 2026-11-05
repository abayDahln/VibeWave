# VibeWave

A lightweight Windows 10/11 desktop audio edge visualizer written in Rust.

VibeWave creates a transparent, click-through overlay along the borders of your display that renders real-time audio reactions (glow or frequency bars) using WASAPI loopback audio and hardware-accelerated Direct2D / DirectComposition. Colors adapt dynamically based on the current media's album artwork via Windows Media Control.

## Features

- **Low Overhead**: The overlay runs entirely in native Rust (Direct2D / DirectComposition). Webview is only spawned on-demand for the settings interface and destroyed on close.
- **Audio Reactive**: Captures system audio output with WASAPI loopback, analyzed with FFT into configurable frequency bands.
- **Media Color Extraction**: Pulls dominant colors from currently playing media (Spotify, browsers, media players) via Windows Media Control.
- **Click-Through Overlay**: Runs transparently on top of other windows without blocking mouse or keyboard input.
- **System Tray**: Quick access to toggles, color profiles, sensitivity sliders, and settings.

## Project Structure

This project is set up as a Cargo workspace:

- `crates/vibewave-core`: Core logic containing DSP analysis, smoothing filters, geometry, and color math. Has no Windows or GUI dependencies and can be tested on any platform.
- `crates/vibewave`: Windows binary handling WASAPI capture, Win32 window management, Direct2D rendering, media integration, and Tauri settings shell.

## Requirements

- Windows 10 or 11 (x86_64)
- Rust 1.78+ (`stable`)
- Visual Studio C++ Build Tools (MSVC toolchain)

## Getting Started

### Clone and Test

```bash
git clone https://github.com/<user>/VibeWave.git
cd VibeWave
cargo test --workspace
```

### Run in Development

```bash
cargo run -p vibewave
```

### Build Release Binary

```bash
cargo build --release -p vibewave
```

The compiled executable will be located at `target/release/vibewave.exe`.

## CLI Usage

```text
Usage: vibewave.exe [OPTIONS]

Options:
  --settings        Open the settings window on launch
  --reset-settings  Reset settings to defaults
  -h, --help        Print help information
```

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
