use std::sync::Arc;
use std::sync::Mutex;

use anyhow::{Context, Result};

use gst;
use gst::prelude::*;
use gst::FlowError;
use gst::State;
use gst_app;
use gst_video;

use wvr_data::types::Buffer;
use wvr_data::types::DataHolder;
use wvr_data::types::InputProvider;

type BgrImage = image::ImageBuffer<image::Bgr<u8>, Vec<u8>>;
type BgraImage = image::ImageBuffer<image::Bgra<u8>, Vec<u8>>;

pub enum TextureFormat {
    RGBU8,
    RGBAU8,
    BGRU8,
    BGRAU8,
}

pub struct CamProvider {
    name: String,
    video_buffer: Arc<Mutex<Buffer>>,
    pipeline: gst::Element,
}

impl CamProvider {
    pub fn new(path: &str, name: String, resolution: (usize, usize)) -> Result<Self> {
        gst::init().expect("Failed to initialize the gstreamer library");

        let video_buffer = Arc::new(Mutex::new(Buffer {
            dimensions: vec![resolution.0, resolution.1, 3],
            data: None,
        }));

        let src = if cfg!(target_os = "linux") {
            format!("v4l2src device={:}", path)
        } else {
            "autovideosrc".to_owned()
        };

        let pipeline_string = format!(
            "{:} ! videoconvert ! videoscale ! video/x-raw,format=RGB,format=RGBA,format=BGR,format=BGRA,width={:},height={:} ! videoflip method=vertical-flip ! appsink name=appsink async=true sync=false",
            src, resolution.0, resolution.1
        );

        let pipeline =
            gst::parse_launch(&pipeline_string).context("Failed to build gstreamer pipeline")?;

        let sink = pipeline
            .clone()
            .dynamic_cast::<gst::Bin>()
            .expect("Failed to cast the gstreamer pipeline as a gst::Bin element")
            .get_by_name("appsink")
            .expect("Failed to retrieve sink from gstreamer pipeline.");

        let appsink = sink
            .dynamic_cast::<gst_app::AppSink>()
            .expect("The sink defined in the pipeline is not an appsink");

        {
            let video_buffer = video_buffer.clone();
            appsink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |appsink| {
                        let sample = match appsink.pull_sample() {
                            Err(e) => {
                                println!("{:}", e);
                                return Err(gst::FlowError::Eos);
                            }
                            Ok(sample) => sample,
                        };

                        let sample_caps = if let Some(sample_caps) = sample.get_caps() {
                            sample_caps
                        } else {
                            return Err(gst::FlowError::Error);
                        };

                        let video_info = if let Ok(video_info) = gst_video::VideoInfo::from_caps(sample_caps) {
                            video_info
                        } else {
                            return Err(gst::FlowError::Error);
                        };

                        let buffer = if let Some(buffer) = sample.get_buffer() {
                            buffer
                        } else {
                            return Err(gst::FlowError::Error);
                        };

                        let map = if let Ok(map) = buffer.map_readable() {
                            map
                        } else {
                            return Err(gst::FlowError::Error);
                        };

                        let samples = map.as_slice().to_vec();
                        let format = match video_info.format() {
                            gst_video::VideoFormat::Rgb => TextureFormat::RGBU8,
                            gst_video::VideoFormat::Rgba => TextureFormat::RGBAU8,
                            gst_video::VideoFormat::Bgr => TextureFormat::BGRU8,
                            gst_video::VideoFormat::Bgra => TextureFormat::BGRAU8,
                            unsupported_format => {
                                eprintln!("Unsupported format: {:?}", unsupported_format);
                                return Err(gst::FlowError::Error);
                            }
                        };

                        let image_buffer = match format {
                            TextureFormat::RGBU8 => image::DynamicImage::ImageRgb8(image::RgbImage::from_raw(video_info.width(), video_info.height(), samples).unwrap()).into_rgb8(),
                            TextureFormat::RGBAU8 => image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(video_info.width(), video_info.height(), samples).unwrap()).into_rgb8(),
                            TextureFormat::BGRU8 => image::DynamicImage::ImageBgr8(BgrImage::from_raw(video_info.width(), video_info.height(), samples).unwrap()).into_rgb8(),
                            TextureFormat::BGRAU8 => image::DynamicImage::ImageBgra8(BgraImage::from_raw(video_info.width(), video_info.height(), samples).unwrap()).into_rgb8(),
                        };

                        match video_buffer.lock() {
                            Ok(mut video_buffer) => {
                                video_buffer.data = Some(image_buffer.into_vec());
                                video_buffer.dimensions = vec![video_info.width() as usize, video_info.height() as usize, 3];
                            }
                            Err(e) => {
                                eprintln!("Could not lock video buffer, did the main thread panic? \n{:?}", e);
                                return Err(FlowError::Error);
                            }
                        }

                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
        }

        pipeline
            .set_state(State::Playing)
            .context("Failed to start gstreamer pipeline")?;

        Ok(Self {
            name,
            video_buffer,
            pipeline,
        })
    }
}

impl Drop for CamProvider {
    fn drop(&mut self) {
        if let Err(e) = self.stop() {
            eprintln!("{:?}", e);
        }
    }
}

impl InputProvider for CamProvider {
    fn set_name(&mut self, name: &str) {
        self.name = name.to_owned();
    }

    fn provides(&self) -> Vec<String> {
        vec![self.name.clone()]
    }

    fn set_property(&mut self, property: &str, _value: &DataHolder) {
        eprintln!("Set_property unimplemented for {:}", property);
    }

    fn get(&mut self, uniform_name: &str, invalidate: bool) -> Option<DataHolder> {
        if uniform_name == self.name {
            if let Ok(mut video_buffer) = self.video_buffer.lock() {
                let result = video_buffer.data.as_ref().map(|data| {
                    DataHolder::Texture((
                        (
                            video_buffer.dimensions[0] as u32,
                            video_buffer.dimensions[1] as u32,
                        ),
                        data.to_vec(),
                    ))
                });

                if invalidate {
                    video_buffer.data = None;
                }

                result
            } else {
                None
            }
        } else {
            None
        }
    }

    fn stop(&mut self) -> Result<()> {
        self.pipeline
            .set_state(State::Null)
            .context("Failed to stop video playback")?;

        Ok(())
    }

    fn play(&mut self) -> Result<()> {
        self.pipeline
            .set_state(State::Playing)
            .context("Failed to resume video playback")?;

        Ok(())
    }
    fn pause(&mut self) -> Result<()> {
        self.pipeline
            .set_state(State::Paused)
            .context("Failed to pause video playback")?;

        Ok(())
    }
}
