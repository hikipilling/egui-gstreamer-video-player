use eframe::egui;
use egui::{TextureHandle, ViewportBuilder};
use glib::{self, MainContext};
use gstreamer as gst;
use gstreamer::bus::BusWatchGuard;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// Custom error type that implements Send + Sync
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

// Structure to hold video frame data
struct VideoFrame {
    width: i32,
    height: i32,
    data: Vec<u8>,
}

// Our custom data structure to hold state
struct MediaPlayer {
    pipeline: gst::Element,
    _appsink: gst_app::AppSink,
    duration: Option<gst::ClockTime>,
    position: Option<gst::ClockTime>,
    video_frame: Arc<Mutex<Option<VideoFrame>>>,
    texture: Option<TextureHandle>,
    _bus_watch: BusWatchGuard,
    main_context: glib::MainContext,
    volume: f64,
}

impl MediaPlayer {
    fn new() -> Result<Self, PlayerError> {
        // Initialize GStreamer
        gst::init().map_err(|e| PlayerError::InitializationError(e.to_string()))?;

        // Create playbin (which is already a pipeline)
        let pipeline = gst::ElementFactory::make("playbin")
            .name("playbin")
            .build()
            .map_err(|e| PlayerError::GstreamerError(format!("Failed to create playbin: {}", e)))?;

        // Create appsink for video
        let appsink = gst_app::AppSink::builder()
            .name("videosink")
            .caps(
                &gst::Caps::builder("video/x-raw")
                    .field("format", "RGBA")
                    .build(),
            )
            .build();

        // Create the video conversion bin
        let video_bin = gst::Bin::new();

        let videoconvert = gst::ElementFactory::make("videoconvert")
            .build()
            .map_err(|e| PlayerError::GstreamerError(e.to_string()))?;

        video_bin.add(&videoconvert).unwrap();
        video_bin.add(appsink.upcast_ref::<gst::Element>()).unwrap();

        // Link elements
        videoconvert
            .link(appsink.upcast_ref::<gst::Element>())
            .unwrap();

        // Create ghost pad
        let sink_pad = videoconvert.static_pad("sink").unwrap();
        let ghost_pad = gst::GhostPad::builder_with_target(&sink_pad)
            .unwrap()
            .name("sink")
            .build();
        video_bin.add_pad(&ghost_pad).unwrap();

        // Set video sink on playbin
        pipeline.set_property("video-sink", &video_bin);

        // Set default URI
        let uri = "file:///home/lain/Downloads/testvideo.mp4".to_string();
        pipeline.set_property("uri", &uri);

        let video_frame = Arc::new(Mutex::new(None));
        let video_frame_clone = Arc::clone(&video_frame);

        // Configure appsink callbacks
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |appsink| {
                    let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Error)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let caps = sample.caps().ok_or(gst::FlowError::Error)?;

                    let video_info =
                        gst_video::VideoInfo::from_caps(caps).map_err(|_| gst::FlowError::Error)?;
                    let width = video_info.width() as i32;
                    let height = video_info.height() as i32;

                    let mapped_buffer = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let data = mapped_buffer.to_vec();

                    *video_frame_clone.lock().unwrap() = Some(VideoFrame {
                        width,
                        height,
                        data,
                    });

                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        // Add bus watch
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
                            if state
                                .src()
                                .map(|s| {
                                    s.as_ptr() == pipeline.upcast_ref::<gst::Object>().as_ptr()
                                })
                                .unwrap_or(false)
                            {
                                println!(
                                    "State changed from {:?} -> {:?}",
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

        let ret = pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| PlayerError::GstreamerError(format!("Failed to play: {}", e)))?;
        println!("Play state change result: {:?}", ret);

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

    fn set_volume(&mut self, volume: f64) {
        self.volume = volume.clamp(0.0, 1.0);
        self.pipeline.set_property("volume", self.volume);
    }

    fn get_state(&self) -> gst::State {
        self.pipeline.current_state()
    }

    fn set_null(&mut self) -> Result<(), PlayerError> {
        let ret = self
            .pipeline
            .set_state(gst::State::Null)
            .map_err(|e| PlayerError::GstreamerError(format!("Failed to set null: {}", e)))?;

        println!("Null state change result: {:?}", ret);
        Ok(())
    }

    fn play(&mut self) -> Result<(), PlayerError> {
        let ret = self
            .pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| PlayerError::GstreamerError(format!("Failed to play: {}", e)))?;

        println!("Play state change result: {:?}", ret);
        Ok(())
    }

    fn pause(&mut self) -> Result<(), PlayerError> {
        let ret = self
            .pipeline
            .set_state(gst::State::Paused)
            .map_err(|e| PlayerError::GstreamerError(format!("Failed to pause: {}", e)))?;

        println!("Pause state change result: {:?}", ret);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), PlayerError> {
        let ret = self
            .pipeline
            .set_state(gst::State::Ready)
            .map_err(|e| PlayerError::GstreamerError(format!("Failed to stop: {}", e)))?;

        println!("Stop state change result: {:?}", ret);
        self.position = Some(gst::ClockTime::ZERO);
        Ok(())
    }

    fn toggle_playback(&mut self) -> Result<(), PlayerError> {
        match self.get_state() {
            gst::State::Playing => self.pause(),
            gst::State::Paused => self.play(),
            gst::State::Ready => self.play(),
            _ => Ok(()),
        }
    }

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
}

impl eframe::App for MediaPlayer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while self.main_context.iteration(false) {}

        let play_button_text = match self.get_state() {
            gst::State::Playing => "⏸",
            _ => "⏵",
        };

        self.update_position();
        self.update_texture(ctx);

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open").clicked() {
                    println!("open file");
                }
                if ui.button("Quit").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });

        egui::TopBottomPanel::bottom("video_controls").show(ctx, |ui| {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                if ui.button(play_button_text).clicked() {
                    println!("{:?}", self.get_state());
                    let _ = self.toggle_playback();
                }
                if ui.button("⏹").clicked() {
                    let _ = self.stop();
                }

                // Position slider
                ui.style_mut().spacing.slider_width = ui.available_width() - 200.0;
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

                ui.horizontal(|ui| {
                    ui.set_width(200.0);
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
                    ui.style_mut().spacing.slider_width = ui.available_width() - 12.0;
                    if ui
                        .add(egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false))
                        .changed()
                    {
                        self.set_volume(volume);
                    }
                });
            });
            ui.add_space(3.0);
        });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(ctx.style().visuals.panel_fill))
            .show(ctx, |ui| {
                if let Some(texture) = &self.texture {
                    let original_size = texture.size_vec2();
                    let aspect_ratio = original_size.x / original_size.y;
                    let mut scaled_height = ui.available_height();
                    let mut scaled_width = scaled_height * aspect_ratio;
                    if scaled_width > ui.available_width() {
                        scaled_width = ui.available_width();
                        scaled_height = scaled_width / aspect_ratio;
                    }
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
                    ui.vertical_centered(|ui| {
                        ui.add_space(ui.available_height() / 2.0);
                        let _ = ui.button("Select file");
                    });
                }
            });

        ctx.request_repaint_after(Duration::from_millis(16)); // ~60 FPS
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.set_null();
    }
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_min_inner_size([400.0, 300.0])
            .with_title("Video Player"),
        ..Default::default()
    };

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
