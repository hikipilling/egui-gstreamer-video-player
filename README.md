# Simple Video Player

A simple video player that demonstrates how to integrate GStreamer with Egui in Rust. It took me AGES to figure out how to do this so I hope it helps someone out :)

## Screenshots

TODO

## Features

- Plays common video formats (mp4, webm, mkv, avi, mov)
- Basic playback controls (play, pause, stop)
- Seeking with progress slider
- Volume control
- Fullscreen mode with auto-hiding controls
- Keyboard shortcuts

## Dependencies

You will need:

- Rust (latest stable)
- GStreamer development libraries
- GTK development libraries (for file dialogs)

Arch:
`sudo pacman -S gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly gst-libav gtk3`

Feel free to submit a pull request for dependency installation on your own system.
