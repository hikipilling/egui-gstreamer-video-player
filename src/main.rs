use eframe::egui;
use egui::{TextureHandle, ViewportBuilder};
use glib::{self, MainContext};
use gstreamer as gst;
use gstreamer::bus::BusWatchGuard;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use rfd::FileDialog;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Custom error type for the video player that can be safely sent between threads
#[derive(Debug)]
enum PlayerError {
    GstreamerError(String),
    InitializationError(String),
}

impl std::error::Error for PlayerError {}

impl fmt::Display for PlayerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlayerError::GstreamerError(msg) => write!(f, "GStreamer error: {}", msg),
            PlayerError::InitializationError(msg) => write!(f, "Initialization error: {}", msg),
        }
    }
}

/// Represents a single frame of video data
struct VideoFrame {
    width: i32,
    height: i32,
    data: Vec<u8>, // RGBA pixel data
}

/// The main media player structure that handles both GStreamer pipeline and UI state
struct MediaPlayer {
    pipeline: gst::Element,                      // The GStreamer playbin pipeline
    _appsink: gst_app::AppSink,                  // Sink element that receives video frames
    duration: Option<gst::ClockTime>,            // Total duration of the current media
    position: Option<gst::ClockTime>,            // Current playback position
    video_frame: Arc<Mutex<Option<VideoFrame>>>, // Current video frame data
    texture: Option<TextureHandle>,              // Egui texture for displaying the video
    _bus_watch: BusWatchGuard,                   // Watch for GStreamer bus messages
    main_context: glib::MainContext,             // GLib main context for event processing
    volume: f64,                                 // Playback volume (0.0 to 1.0)
}

impl MediaPlayer {
    /// Creates a new MediaPlayer instance, initializing the GStreamer pipeline
    /// and setting up video processing
    fn new() -> Result<Self, PlayerError> {
        // Initialize GStreamer
        gst::init().map_err(|e| PlayerError::InitializationError(e.to_string()))?;

        // Create playbin element (an all-in-one media player pipeline)
        let pipeline = gst::ElementFactory::make("playbin")
            .name("playbin")
            .build()
            .map_err(|e| PlayerError::GstreamerError(format!("Failed to create playbin: {}", e)))?;

        // Create appsink for receiving video frames
        // Configure it to receive RGBA video for easy display in Egui
        let appsink = gst_app::AppSink::builder()
            .name("videosink")
            .caps(
                &gst::Caps::builder("video/x-raw")
                    .field("format", "RGBA")
                    .build(),
            )
            .build();

        // Create a bin for video conversion
        // This ensures we get the video format we want (RGBA)
        let video_bin = gst::Bin::new();
        let videoconvert = gst::ElementFactory::make("videoconvert")
            .build()
            .map_err(|e| PlayerError::GstreamerError(e.to_string()))?;

        video_bin.add(&videoconvert).unwrap();
        video_bin.add(appsink.upcast_ref::<gst::Element>()).unwrap();

        // Link the converter to the sink
        videoconvert
            .link(appsink.upcast_ref::<gst::Element>())
            .unwrap();

        // Create a ghost pad to expose the videoconvert sink pad on the bin
        let sink_pad = videoconvert.static_pad("sink").unwrap();
        let ghost_pad = gst::GhostPad::builder_with_target(&sink_pad)
            .unwrap()
            .name("sink")
            .build();
        video_bin.add_pad(&ghost_pad).unwrap();

        // Configure the pipeline to use our video processing bin
        pipeline.set_property("video-sink", &video_bin);

        // Set up shared storage for video frames
        let video_frame = Arc::new(Mutex::new(None));
        let video_frame_clone = Arc::clone(&video_frame);

        // Configure the appsink to handle incoming video frames
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |appsink| {
                    let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Error)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let caps = sample.caps().ok_or(gst::FlowError::Error)?;

                    // Get video dimensions from the caps
                    let video_info =
                        gst_video::VideoInfo::from_caps(caps).map_err(|_| gst::FlowError::Error)?;
                    let width = video_info.width() as i32;
                    let height = video_info.height() as i32;

                    // Copy frame data
                    let mapped_buffer = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let data = mapped_buffer.to_vec();

                    // Store the frame for later display
                    *video_frame_clone.lock().unwrap() = Some(VideoFrame {
                        width,
                        height,
                        data,
                    });

                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        // Set up bus watch to handle pipeline messages
        let pipeline_weak = pipeline.downgrade();
        let bus = pipeline.bus().unwrap();
        let bus_watch = bus
            .add_watch(move |_, msg| {
                if let Some(pipeline) = pipeline_weak.upgrade() {
                    match msg.view() {
                        gst::MessageView::Error(err) => {
                            eprintln!(
                                "Error from {:?}: {} ({:?})",
                                err.src().map(|s| s.path_string()),
                                err.error(),
                                err.debug()
                            );
                        }
                        gst::MessageView::Eos(_) => {
                            println!("End of stream reached");
                            let _ = pipeline.set_state(gst::State::Ready);
                        }
                        gst::MessageView::StateChanged(state) => {
                            // Only print state changes of the pipeline
                            if state
                                .src()
                                .map(|s| {
                                    s.as_ptr() == pipeline.upcast_ref::<gst::Object>().as_ptr()
                                })
                                .unwrap_or(false)
                            {
                                println!(
                                    "Pipeline state changed: {:?} -> {:?}",
                                    state.old(),
                                    state.current()
                                );
                            }
                        }
                        _ => (),
                    }
                }
                glib::ControlFlow::Continue
            })
            .expect("Failed to add bus watch");

        Ok(MediaPlayer {
            pipeline,
            _appsink: appsink,
            duration: None,
            position: None,
            video_frame,
            texture: None,
            _bus_watch: bus_watch,
            main_context: MainContext::default(),
            volume: 1.0,
        })
    }

    /// Opens a file dialog for the user to select a video file
    fn select_file(&mut self) -> Result<(), PlayerError> {
        if let Some(path) = FileDialog::new()
            .add_filter("Video", &["mp4", "webm", "mkv", "avi"])
            .pick_file()
        {
            self.load_file(path)?;
        }
        Ok(())
    }

    /// Loads and starts playing a video file from the given path
    fn load_file(&mut self, path: PathBuf) -> Result<(), PlayerError> {
        self.stop()?;
        let uri = format!("file://{}", path.to_str().unwrap_or(""));
        self.pipeline.set_property("uri", &uri);
        self.duration = None;
        self.position = Some(gst::ClockTime::ZERO);
        self.play()?;
        Ok(())
    }

    /// Sets the playback volume (0.0 to 1.0)
    fn set_volume(&mut self, volume: f64) {
        self.volume = volume.clamp(0.0, 1.0);
        self.pipeline.set_property("volume", self.volume);
    }

    /// Gets the current state of the pipeline
    fn get_state(&self) -> gst::State {
        self.pipeline.current_state()
    }

    /// Sets the pipeline to NULL state (completely stopped)
    fn set_null(&mut self) -> Result<(), PlayerError> {
        let ret = self
            .pipeline
            .set_state(gst::State::Null)
            .map_err(|e| PlayerError::GstreamerError(format!("Failed to set null: {}", e)))?;
        println!("Pipeline set to NULL state: {:?}", ret);
        Ok(())
    }

    /// Starts or resumes playback
    fn play(&mut self) -> Result<(), PlayerError> {
        let ret = self
            .pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| PlayerError::GstreamerError(format!("Failed to play: {}", e)))?;
        println!("Pipeline set to PLAYING state: {:?}", ret);
        Ok(())
    }

    /// Pauses playback
    fn pause(&mut self) -> Result<(), PlayerError> {
        let ret = self
            .pipeline
            .set_state(gst::State::Paused)
            .map_err(|e| PlayerError::GstreamerError(format!("Failed to pause: {}", e)))?;
        println!("Pipeline set to PAUSED state: {:?}", ret);
        Ok(())
    }

    /// Stops playback and resets position
    fn stop(&mut self) -> Result<(), PlayerError> {
        let ret = self
            .pipeline
            .set_state(gst::State::Ready)
            .map_err(|e| PlayerError::GstreamerError(format!("Failed to stop: {}", e)))?;
        println!("Pipeline set to READY state: {:?}", ret);
        self.position = Some(gst::ClockTime::ZERO);
        Ok(())
    }

    /// Toggles between playing and paused states
    fn toggle_playback(&mut self) -> Result<(), PlayerError> {
        match self.get_state() {
            gst::State::Playing => self.pause(),
            gst::State::Paused | gst::State::Ready => self.play(),
            _ => Ok(()),
        }
    }

    /// Updates the current playback position and duration
    fn update_position(&mut self) {
        if let Some(position) = self.pipeline.query_position::<gst::ClockTime>() {
            self.position = Some(position);
        }

        if self.duration.is_none() {
            if let Some(duration) = self.pipeline.query_duration::<gst::ClockTime>() {
                self.duration = Some(duration);
            }
        }
    }

    /// Seeks to a specific position (0.0 to 1.0) in the video
    fn seek(&mut self, position: f64) -> Result<(), PlayerError> {
        if let Some(duration) = self.duration {
            let position = (position * duration.nseconds() as f64) as i64;
            self.pipeline
                .seek_simple(
                    gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                    gst::ClockTime::from_nseconds(position as u64),
                )
                .map_err(|e| PlayerError::GstreamerError(format!("Failed to seek: {}", e)))?;
        }
        Ok(())
    }

    /// Updates the Egui texture with the current video frame
    fn update_texture(&mut self, ctx: &egui::Context) {
        if let Some(frame) = self.video_frame.lock().unwrap().as_ref() {
            self.texture = Some(ctx.load_texture(
                "video-frame",
                egui::ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    &frame.data,
                ),
                egui::TextureOptions::LINEAR,
            ));
        }
    }

    /// Toggles fullscreen mode
    fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        let is_fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!is_fullscreen));
    }

    /// Exits fullscreen mode
    fn fullscreen_off(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
    }

    /// Checks if the player is currently in fullscreen mode
    fn is_fullscreen(&mut self, ctx: &egui::Context) -> bool {
        ctx.input(|i| i.viewport().fullscreen.unwrap_or(false))
    }
}

/// Implementation of the Egui App trait for our MediaPlayer
impl eframe::App for MediaPlayer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Process any pending GStreamer events
        while self.main_context.iteration(false) {}

        // Handle keyboard shortcuts
        if ctx.input(|i| i.key_pressed(egui::Key::Space)) {
            let _ = self.toggle_playback();
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.fullscreen_off(ctx);
        }

        if ctx.input(|i| i.key_pressed(egui::Key::F11)) {
            println!("F11 pressed - toggling fullscreen");
            self.toggle_fullscreen(ctx);
        }

        // Determine the play/pause button text based on current state
        let play_button_text = match self.get_state() {
            gst::State::Playing => "⏸",
            _ => "⏵",
        };

        // Auto-hide controls in fullscreen mode after 3 seconds of inactivity
        let inactive = ctx.input(|i| i.pointer.time_since_last_movement() > 3.0);
        let controls_shown = !self.is_fullscreen(ctx) || !inactive;
        if inactive {
            ctx.output_mut(|o| o.cursor_icon = egui::CursorIcon::None);
        }

        // Keep our state updated
        self.update_position();
        self.update_texture(ctx);

        // Create the top menu bar
        egui::TopBottomPanel::top("top_panel").show_animated(ctx, controls_shown, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open file").clicked() {
                        if let Err(e) = self.select_file() {
                            eprintln!("Error selecting file: {}", e);
                        }
                        ui.close_menu();
                    }
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("View", |ui| {
                    if ui.button("Toggle fullscreen").clicked() {
                        self.toggle_fullscreen(ctx);
                        ui.close_menu();
                    }
                });
            });
        });

        // Create the bottom control panel with playback controls
        egui::TopBottomPanel::bottom("video_controls").show_animated(ctx, controls_shown, |ui| {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                // Play/Pause and Stop buttons
                if ui.button(play_button_text).clicked() {
                    let _ = self.toggle_playback();
                }
                if ui.button("⏹").clicked() {
                    let _ = self.stop();
                }

                // Position slider
                ui.style_mut().spacing.slider_width = ui.available_width() - 240.0;
                if let (Some(position), Some(duration)) = (self.position, self.duration) {
                    let mut pos = position.seconds() as f64 / duration.seconds() as f64;
                    if ui
                        .add(egui::Slider::new(&mut pos, 0.0..=1.0).show_value(false))
                        .changed()
                    {
                        let _ = self.seek(pos);
                    }
                } else {
                    let mut pos = 0.0;
                    ui.add(
                        egui::Slider::new(&mut pos, 0.0..=1.0)
                            .step_by(0.0)
                            .show_value(false),
                    );
                }

                // Time display and volume controls
                ui.horizontal(|ui| {
                    ui.set_width(240.0);
                    if let (Some(position), Some(duration)) = (self.position, self.duration) {
                        ui.label(format!(
                            "{:02}:{:02} / {:02}:{:02}",
                            position.seconds() / 60,
                            position.seconds() % 60,
                            duration.seconds() / 60,
                            duration.seconds() % 60
                        ));
                    } else {
                        ui.label("00:00 / 00:00");
                    }
                    ui.separator();
                    ui.label("🔊");
                    let mut volume = self.volume;
                    ui.style_mut().spacing.slider_width = ui.available_width() - 36.0;
                    if ui
                        .add(egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false))
                        .changed()
                    {
                        self.set_volume(volume);
                    }
                    if ui.button("🗖").clicked() {
                        self.toggle_fullscreen(ctx);
                    }
                });
            });
            ui.add_space(3.0);
        });

        // Main video display area
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(ctx.style().visuals.panel_fill))
            .show(ctx, |ui| {
                if let Some(texture) = &self.texture {
                    // Calculate video dimensions while maintaining aspect ratio
                    let original_size = texture.size_vec2();
                    let aspect_ratio = original_size.x / original_size.y;
                    let mut scaled_height = ui.available_height();
                    let mut scaled_width = scaled_height * aspect_ratio;
                    if scaled_width > ui.available_width() {
                        scaled_width = ui.available_width();
                        scaled_height = scaled_width / aspect_ratio;
                    }

                    // Display the video frame
                    egui::Frame::none()
                        .fill(egui::Color32::BLACK)
                        .show(ui, |ui| {
                            ui.centered_and_justified(|ui| {
                                ui.add(egui::Image::new((
                                    texture.id(),
                                    egui::vec2(scaled_width, scaled_height),
                                )));
                            });
                        });
                } else {
                    // Show file selection button when no video is loaded
                    ui.vertical_centered(|ui| {
                        ui.add_space(ui.available_height() / 2.0);
                        if ui.button("Select file").clicked() {
                            if let Err(e) = self.select_file() {
                                eprintln!("Error selecting file: {}", e);
                            }
                        }
                    });
                }
            });

        // Request continuous updates for smooth playback
        ctx.request_repaint_after(Duration::from_millis(16)); // ~60 FPS
    }

    /// Set state to NULL on exit to prevent gstreamer memory leaks
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.set_null();
    }
}

/// Main entry point for the application
fn main() -> Result<(), eframe::Error> {
    // Configure the application window
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_min_inner_size([400.0, 300.0])
            .with_title("Video Player"),
        ..Default::default()
    };

    // Start the application
    eframe::run_native(
        "Video Player",
        options,
        Box::new(
            |_cc| -> Result<
                Box<dyn eframe::App>,
                Box<dyn std::error::Error + Send + Sync + 'static>,
            > {
                let player = MediaPlayer::new()?;
                Ok(Box::new(player))
            },
        ),
    )
}
