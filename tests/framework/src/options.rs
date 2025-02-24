use crate::backends::TestAudioBackend;
use crate::environment::{Environment, RenderInterface};
use crate::image_trigger::ImageTrigger;
use anyhow::{anyhow, Result};
use approx::assert_relative_eq;
use regex::Regex;
use ruffle_core::tag_utils::SwfMovie;
use ruffle_core::{PlayerBuilder, ViewportDimensions};
use ruffle_render::backend::RenderBackend;
use ruffle_render::quality::StageQuality;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestOptions {
    pub num_frames: Option<u32>,
    pub num_ticks: Option<u32>,
    pub tick_rate: Option<f64>,
    pub output_path: PathBuf,
    pub sleep_to_meet_frame_rate: bool,
    pub image_comparisons: HashMap<String, ImageComparison>,
    pub ignore: bool,
    pub known_failure: bool,
    pub approximations: Option<Approximations>,
    pub player_options: PlayerOptions,
    pub log_fetch: bool,
    pub required_features: RequiredFeatures,
}

impl Default for TestOptions {
    fn default() -> Self {
        Self {
            num_frames: None,
            num_ticks: None,
            tick_rate: None,
            output_path: PathBuf::from("output.txt"),
            sleep_to_meet_frame_rate: false,
            image_comparisons: Default::default(),
            ignore: false,
            known_failure: false,
            approximations: None,
            player_options: PlayerOptions::default(),
            log_fetch: false,
            required_features: RequiredFeatures::default(),
        }
    }
}

impl TestOptions {
    pub fn read<P: AsRef<Path>>(path: P) -> Result<Self> {
        let result: Self = toml::from_str(&fs::read_to_string(path)?)?;
        result.validate()?;
        Ok(result)
    }

    fn validate(&self) -> Result<()> {
        if !self.image_comparisons.is_empty() {
            let mut seen_triggers = HashSet::new();
            for comparison in self.image_comparisons.values() {
                if comparison.trigger != ImageTrigger::FsCommand
                    && !seen_triggers.insert(comparison.trigger)
                {
                    return Err(anyhow!(
                        "Multiple captures are set to trigger {:?}. This likely isn't intended!",
                        comparison.trigger
                    ));
                }
            }
        }

        Ok(())
    }

    pub fn output_path(&self, test_directory: &Path) -> PathBuf {
        test_directory.join(&self.output_path)
    }
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Approximations {
    number_patterns: Vec<String>,
    epsilon: Option<f64>,
    max_relative: Option<f64>,
}

impl Approximations {
    pub fn compare(&self, actual: f64, expected: f64) {
        match (self.epsilon, self.max_relative) {
            (Some(epsilon), Some(max_relative)) => assert_relative_eq!(
                actual,
                expected,
                epsilon = epsilon,
                max_relative = max_relative
            ),
            (Some(epsilon), None) => assert_relative_eq!(actual, expected, epsilon = epsilon),
            (None, Some(max_relative)) => {
                assert_relative_eq!(actual, expected, max_relative = max_relative)
            }
            (None, None) => assert_relative_eq!(actual, expected),
        }
    }

    pub fn number_patterns(&self) -> Vec<Regex> {
        self.number_patterns
            .iter()
            .map(|p| Regex::new(p).unwrap())
            .collect()
    }
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct RequiredFeatures {
    lzma: bool,
    jpegxr: bool,
}

impl RequiredFeatures {
    pub fn can_run(&self) -> bool {
        (!self.lzma || cfg!(feature = "lzma")) && (!self.jpegxr || cfg!(feature = "jpegxr"))
    }
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct PlayerOptions {
    max_execution_duration: Option<Duration>,
    viewport_dimensions: Option<ViewportDimensions>,
    with_renderer: Option<RenderOptions>,
    with_audio: bool,
    with_video: bool,
}

impl PlayerOptions {
    pub fn setup(&self, mut player_builder: PlayerBuilder) -> Result<PlayerBuilder> {
        if let Some(max_execution_duration) = self.max_execution_duration {
            player_builder = player_builder.with_max_execution_duration(max_execution_duration);
        }

        if let Some(render_options) = &self.with_renderer {
            player_builder = player_builder.with_quality(match render_options.sample_count {
                16 => StageQuality::High16x16,
                8 => StageQuality::High8x8,
                4 => StageQuality::High,
                2 => StageQuality::Medium,
                _ => StageQuality::Low,
            });
        }

        if self.with_audio {
            player_builder = player_builder.with_audio(TestAudioBackend::default());
        }

        #[cfg(feature = "ruffle_video_software")]
        if self.with_video {
            use ruffle_video_software::backend::SoftwareVideoBackend;
            player_builder = player_builder.with_video(SoftwareVideoBackend::new())
        }

        Ok(player_builder)
    }

    pub fn can_run(&self, check_renderer: bool, environment: &impl Environment) -> bool {
        if let Some(render) = &self.with_renderer {
            // If we don't actually want to check the renderer (ie we're just listing potential tests),
            // don't spend the cost to create it
            if check_renderer && !render.optional && !environment.is_render_supported(render) {
                return false;
            }
        }
        true
    }

    pub fn viewport_dimensions(&self, movie: &SwfMovie) -> ViewportDimensions {
        self.viewport_dimensions
            .unwrap_or_else(|| ViewportDimensions {
                width: movie.width().to_pixels() as u32,
                height: movie.height().to_pixels() as u32,
                scale_factor: 1.0,
            })
    }

    pub fn create_renderer(
        &self,
        environment: &impl Environment,
        dimensions: ViewportDimensions,
    ) -> Vec<(Box<dyn RenderInterface>, Box<dyn RenderBackend>)> {
        if self.with_renderer.is_some() {
            environment.create_renderers(dimensions.width, dimensions.height)
        } else {
            vec![]
        }
    }
}

#[derive(Deserialize, Default, Clone, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct ImageComparison {
    tolerance: u8,
    max_outliers: usize,
    pub trigger: ImageTrigger,
}

fn calc_difference(lhs: u8, rhs: u8) -> u8 {
    (lhs as i16 - rhs as i16).unsigned_abs() as u8
}

impl ImageComparison {
    pub fn test(
        &self,
        name: &str,
        actual_image: image::RgbaImage,
        expected_image: image::RgbaImage,
        test_path: &Path,
        environment_name: String,
        known_failure: bool,
    ) -> Result<()> {
        use anyhow::Context;

        let save_actual_image = || {
            if !known_failure {
                // If we're expecting failure, spamming files isn't productive.
                actual_image
                    .save(test_path.join(format!("{name}.actual-{environment_name}.png")))
                    .context("Couldn't save actual image")
            } else {
                Ok(())
            }
        };

        if actual_image.width() != expected_image.width()
            || actual_image.height() != expected_image.height()
        {
            save_actual_image()?;
            return Err(anyhow!(
                "'{}' image is not the right size. Expected = {}x{}, actual = {}x{}.",
                name,
                expected_image.width(),
                expected_image.height(),
                actual_image.width(),
                actual_image.height()
            ));
        }

        let mut is_alpha_different = false;

        let difference_data: Vec<u8> = expected_image
            .as_raw()
            .chunks_exact(4)
            .zip(actual_image.as_raw().chunks_exact(4))
            .flat_map(|(cmp_chunk, data_chunk)| {
                if cmp_chunk[3] != data_chunk[3] {
                    is_alpha_different = true;
                }

                [
                    calc_difference(cmp_chunk[0], data_chunk[0]),
                    calc_difference(cmp_chunk[1], data_chunk[1]),
                    calc_difference(cmp_chunk[2], data_chunk[2]),
                    calc_difference(cmp_chunk[3], data_chunk[3]),
                ]
            })
            .collect();

        let outliers: usize = difference_data
            .chunks_exact(4)
            .map(|colors| {
                (colors[0] > self.tolerance) as usize
                    + (colors[1] > self.tolerance) as usize
                    + (colors[2] > self.tolerance) as usize
                    + (colors[3] > self.tolerance) as usize
            })
            .sum();

        let max_difference = difference_data
            .chunks_exact(4)
            .map(|colors| colors[0].max(colors[1]).max(colors[2]).max(colors[3]))
            .max()
            .unwrap();

        if outliers > self.max_outliers {
            save_actual_image()?;

            let mut difference_color = Vec::with_capacity(
                actual_image.width() as usize * actual_image.height() as usize * 3,
            );
            for p in difference_data.chunks_exact(4) {
                difference_color.extend_from_slice(&p[..3]);
            }

            if !known_failure {
                // If we're expecting failure, spamming files isn't productive.
                image::RgbImage::from_raw(
                    actual_image.width(),
                    actual_image.height(),
                    difference_color,
                )
                .context("Couldn't create color difference image")?
                .save(test_path.join(format!("{name}.difference-color-{environment_name}.png")))
                .context("Couldn't save color difference image")?;
            }

            if is_alpha_different {
                let mut difference_alpha = Vec::with_capacity(
                    actual_image.width() as usize * actual_image.height() as usize,
                );
                for p in difference_data.chunks_exact(4) {
                    difference_alpha.push(p[3])
                }

                if !known_failure {
                    // If we're expecting failure, spamming files isn't productive.
                    image::GrayImage::from_raw(
                        actual_image.width(),
                        actual_image.height(),
                        difference_alpha,
                    )
                    .context("Couldn't create alpha difference image")?
                    .save(test_path.join(format!("{name}.difference-alpha-{environment_name}.png")))
                    .context("Couldn't save alpha difference image")?;
                }
            }

            return Err(anyhow!(
                "Image '{}' failed: Number of outliers ({}) is bigger than allowed limit of {}. Max difference is {}",
                name,
                outliers,
                self.max_outliers,
                max_difference
            ));
        } else {
            println!("Image '{name}' succeeded: {outliers} outliers found, max difference {max_difference}",);
        }

        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RenderOptions {
    optional: bool,
    pub sample_count: u32,
    pub exclude_warp: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            optional: false,
            sample_count: 1,
            exclude_warp: false,
        }
    }
}
