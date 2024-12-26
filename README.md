# Simple Video Player

A simple, performant video player that demonstrates how to integrate GStreamer with Egui in Rust. It took me AGES to figure out how to do this so I hope it helps someone out :)

## Screenshots

![video playing](https://github.com/user-attachments/assets/838a0ea9-6915-4d46-bf01-b0174b5e340c)
![no video selected](https://github.com/user-attachments/assets/3349aa23-d932-4787-84b2-dbf9e6022da7)

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
```
sudo pacman -S gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly gst-libav gtk3
```

Feel free to submit a pull request for dependency installation on your own system.
