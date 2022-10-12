use crate::model::Config;
use jarvis_lib::measurement_client::MeasurementClient;
use jarvis_lib::model::{EntityType, Measurement, MetricType, Sample, SampleType};

use chrono::Utc;
use log::info;
use std::error::Error;
use uuid::Uuid;

pub struct IdleClient {}

impl MeasurementClient<Config> for IdleClient {
    fn get_measurement(
        &self,
        config: Config,
        last_measurement: Option<Measurement>,
    ) -> Result<Measurement, Box<dyn Error>> {
        info!("Writing measurement from idle config...");

        let mut measurement = Measurement {
            id: Uuid::new_v4().to_string(),
            source: String::from("jarvis-tesla-exporter"),
            location: config.location.clone(),
            samples: Vec::new(),
            measured_at_time: Utc::now(),
        };

        for sample_config in config.sample_configs {
            let instance_count = if let Some(instance_count) = sample_config.instance_count {
                instance_count as f64
            } else {
                1_f64
            };

            // get previous counter value to have a continuously increasing counter
            let last_counter_value: f64 = if let Some(last_measurement) = last_measurement.as_ref()
            {
                if let Some(sample) = last_measurement.samples.iter().find(|s| {
                    s.sample_name == sample_config.sample_name
                        && s.metric_type == MetricType::Counter
                }) {
                    sample.value
                } else {
                    0_f64
                }
            } else {
                0_f64
            };

            // store as gauge for timeline graphs
            measurement.samples.push(Sample {
                entity_type: EntityType::Device,
                entity_name: "jarvis-tesla-exporter".into(),
                sample_type: SampleType::ElectricityConsumption,
                sample_name: sample_config.sample_name.clone(),
                metric_type: MetricType::Gauge,
                value: sample_config.value_watt * instance_count,
            });

            // store as counter for totals
            measurement.samples.push(Sample {
                entity_type: EntityType::Device,
                entity_name: "jarvis-tesla-exporter".into(),
                sample_type: SampleType::ElectricityConsumption,
                sample_name: sample_config.sample_name,
                metric_type: MetricType::Counter,
                value: last_counter_value
                    + sample_config.value_watt * instance_count * config.interval_seconds,
            });
        }

        Ok(measurement)
    }
}

impl IdleClient {
    pub fn new() -> Self {
        Self {}
    }
}
